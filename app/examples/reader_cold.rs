//! First HTML preview after creating and reopening a window, plus deterministic slow-layout
//! checks. Uses the real reader and window on a private Broadway display; no account/network.
//!
//! ```sh
//! cargo build --locked -p pronto --bin pronto --example reader_cold
//! tools/headless.sh 95 target/debug/examples/reader_cold
//! ```
//!
//! `COLD` lines report window creation → click → shown → painted. A held first layout must
//! reveal progressively within one second; a direct replacement retains the old conversation
//! until the existing three-second cap; an explicit selection move keeps its 150 ms cap.

#[path = "reader_support/mod.rs"]
mod support;

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use sealwax_app::reader::{Reader, Shown, ShownBody, samples};
use sealwax_app::theme::{STARTUP_WAIT, ThemeController};
use sealwax_app::window::Window;
use sealwax_domain::model::MessageId;
use sealwax_domain::theme::ThemeOptions;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const FIRST_LIMIT: Duration = Duration::from_secs(1);

fn main() -> glib::ExitCode {
    support::count_criticals();
    let _ = sealwax_infrastructure::logging::init();
    let input = match support::broadway::connect() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("run this through tools/headless.sh: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let scratch = Rc::new(support::store(&support::scratch("cold")));
    let filled = samples::fill_store(
        &scratch.store,
        &scratch.blobs,
        samples::messages(glib::real_time() / 1_000_000),
    )
    .expect("synthetic reader samples");
    let app = adw::Application::builder()
        .application_id("io.github.omarsimon1.Pronto.ReaderCold")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let passed = Rc::new(Cell::new(false));
    app.connect_activate(glib::clone!(
        #[strong]
        passed,
        move |app| {
            let hold = app.hold();
            let app = app.clone();
            let passed = passed.clone();
            let scratch = scratch.clone();
            let filled = filled.clone();
            let input = input.clone();
            glib::spawn_future_local(async move {
                let _keep = (hold, input);
                let theme = ThemeController::start(ThemeOptions::default(), STARTUP_WAIT);
                let result = scenario(&app, &scratch, &theme, &filled).await;
                let ok = match result {
                    Ok(ok) => ok,
                    Err(error) => {
                        println!("COLD error {error}");
                        false
                    }
                };
                println!("COLD GLib criticals={}", support::criticals());
                passed.set(ok && support::criticals() == 0);
                app.quit();
            });
        }
    ));
    app.run_with_args::<&str>(&[]);
    if passed.get() {
        glib::ExitCode::SUCCESS
    } else {
        glib::ExitCode::FAILURE
    }
}

async fn scenario(
    app: &adw::Application,
    scratch: &support::Scratch,
    theme: &Rc<ThemeController>,
    filled: &samples::Filled,
) -> Result<bool, String> {
    let mut ok = true;
    for (label, sample) in [("fresh", "simple"), ("reopened", "newsletter")] {
        let opened = Instant::now();
        let reader = support::reader(scratch, theme);
        let window = support::window(app, &reader, (1440, 900));
        warm_window(&reader, &window);
        window.present();
        support::frames(window.window(), 2).await?;
        // A short, fixed gap represents the user reaching the first message after open.
        support::sleep(250).await;
        let reports = support::shown_reports(&reader);
        println!(
            "COLD {label} views_before_click={}",
            reader.state().live_views
        );
        let clicked = Instant::now();
        let shown = timed_show(
            label,
            &reader,
            &window,
            &reports,
            filled.id(sample).ok_or("missing sample")?,
            opened,
        )
        .await?;
        ok &= shown.ready < FIRST_LIMIT && matches!(shown.body, ShownBody::Html { .. });
        // Progressive visibility alone is not enough: the actual HTML must finish rendering.
        if !support::wait_until(Duration::from_secs(5), || {
            reader
                .bodies()
                .iter()
                .any(|body| body.loaded && body.painted)
        })
        .await
        {
            return Err(format!("{label}: HTML never loaded and painted"));
        }
        println!(
            "COLD {label} click_to_html_ms={:.1}",
            clicked.elapsed().as_secs_f64() * 1000.0
        );
        close_window(&reader, &window).await;
        // Both the reader and the window are dropped at the loop boundary, just as when the
        // background app closes its last window and later builds a new one.
    }

    let opened = Instant::now();
    let reader = support::reader(scratch, theme);
    let window = support::window(app, &reader, (1440, 900));
    warm_window(&reader, &window);
    window.present();
    support::frames(window.window(), 2).await?;
    let reports = support::shown_reports(&reader);
    reader.hold_layout(true);
    let simple = filled.id("simple").ok_or("missing simple")?;
    let shown = timed_show("held-first", &reader, &window, &reports, simple, opened).await?;
    ok &= (Duration::from_millis(400)..FIRST_LIMIT).contains(&shown.ready);

    let newsletter = filled.id("newsletter").ok_or("missing newsletter")?;
    let started = Instant::now();
    reader.show(newsletter);
    support::sleep(650).await;
    let retained = reader.message() == Some(simple);
    let shown = report(&reports, newsletter).await?;
    println!(
        "COLD held-replacement retained_at_650ms={retained} shown_ms={:.1}",
        shown.ready.as_secs_f64() * 1000.0
    );
    ok &= retained && started.elapsed() >= Duration::from_millis(2900);

    let cid = filled.id("cid").ok_or("missing cid")?;
    reader.show_next(cid);
    let shown = report(&reports, cid).await?;
    println!(
        "COLD held-selection-move shown_ms={:.1}",
        shown.ready.as_secs_f64() * 1000.0
    );
    ok &= (Duration::from_millis(100)..FIRST_LIMIT).contains(&shown.ready);

    // Clearing leaves an empty pane again; it must receive the same short layout budget.
    reader.clear();
    let shown = timed_show(
        "held-after-clear",
        &reader,
        &window,
        &reports,
        simple,
        opened,
    )
    .await?;
    ok &= (Duration::from_millis(400)..FIRST_LIMIT).contains(&shown.ready);
    reader.hold_layout(false);
    close_window(&reader, &window).await;
    println!(
        "COLD {}",
        if ok {
            "all expectations held"
        } else {
            "FAILED"
        }
    );
    Ok(ok)
}

// Keep lifecycle wiring together so the same measurements can also be compiled against a
// previous build, with just these adapters changed to that build's window lifecycle.
fn warm_window(reader: &Reader, window: &Window) {
    reader.prewarm_in(window.dock().widget());
}

async fn close_window(reader: &Reader, window: &Window) {
    reader.rest();
    window.window().close();
    support::sleep(100).await;
    println!("COLD closed views={}", reader.state().live_views);
    assert!(
        reader.state().live_views <= 2,
        "only the small warm pool remains"
    );
}

async fn timed_show(
    label: &str,
    reader: &Reader,
    window: &Window,
    reports: &async_channel::Receiver<Shown>,
    id: MessageId,
    opened: Instant,
) -> Result<Shown, String> {
    let clicked = Instant::now();
    let shown = support::show(reader, reports, id).await?;
    support::frames(window.window(), 1).await?;
    println!(
        "COLD {label} open_to_click_ms={:.1} click_to_shown_ms={:.1} click_to_painted_ms={:.1}",
        clicked.duration_since(opened).as_secs_f64() * 1000.0,
        shown.ready.as_secs_f64() * 1000.0,
        clicked.elapsed().as_secs_f64() * 1000.0,
    );
    Ok(shown)
}

async fn report(reports: &async_channel::Receiver<Shown>, id: MessageId) -> Result<Shown, String> {
    loop {
        let shown = glib::future_with_timeout(Duration::from_secs(5), reports.recv())
            .await
            .map_err(|_| "conversation never shown")?
            .map_err(|error| error.to_string())?;
        if shown.id == id {
            return Ok(shown);
        }
    }
}
