use gtk4::prelude::*;
use gtk4::{gio, glib};
use sealwax_app::application::{self, AppConfig};
use sealwax_app::{activation, cli, config, selftest};
use std::time::Instant;

fn main() -> glib::ExitCode {
    let started = Instant::now();
    let sandbox_off = std::env::var_os(config::SANDBOX_OFF_ENV).is_some();
    if sandbox_off {
        // SAFETY: Pronto has one thread here (GTK, WebKit, the logger and the engine start
        // theirs later), so nothing else reads or writes the environment meanwhile.
        unsafe { std::env::remove_var(config::SANDBOX_OFF_ENV) };
    }
    // GLib sends the process name as a notification's app_name (S6).
    glib::set_prgname(Some("pronto"));
    glib::set_application_name("Pronto");
    // Parsed here first, so a typo never reaches a running Pronto.
    let parsed = match cli::parse(std::env::args_os()) {
        Ok(parsed) => parsed,
        Err(error) => {
            let _ = error.print();
            return glib::ExitCode::from(error.exit_code() as u8);
        }
    };
    // Migrate before logging or settings readers can create the new XDG directories.
    // Test scripts and custom app IDs must never read a real account's files.
    if application::app_id_from_env() == "io.github.omarsimon1.Pronto"
        && parsed.self_test.is_none()
        && let Err(error) = migrate()
    {
        eprintln!("Could not copy Dispatchy data to Pronto: {error}");
        return glib::ExitCode::FAILURE;
    }
    if let Err(error) = sealwax_infrastructure::logging::init() {
        eprintln!("Could not initialize logging: {error}");
        return glib::ExitCode::FAILURE;
    }
    if sandbox_off {
        tracing::warn!(
            "{} was set; Pronto cleared it, so WebKit's sandbox stays on",
            config::SANDBOX_OFF_ENV
        );
    }
    let exit_after = match config::exit_after_from_env() {
        Ok(exit_after) => exit_after,
        Err(message) => {
            tracing::error!("{message}");
            return glib::ExitCode::FAILURE;
        }
    };
    if let Some(script) = parsed.self_test {
        return selftest::run(&script, started);
    }
    let config = AppConfig::from_env(exit_after);
    if !parsed.service {
        let outcome = activation::prepare(&config.app_id);
        tracing::debug!(?outcome, "command line");
    }
    application::run(config, started)
}

fn migrate() -> std::io::Result<()> {
    migrate_dirs(&sealwax_infrastructure::xdg::SealwaxDirs::from_env())
}

fn migrate_dirs(dirs: &sealwax_infrastructure::xdg::SealwaxDirs) -> std::io::Result<()> {
    use sealwax_infrastructure::migration;
    if !migration::needed(dirs)? {
        return Ok(());
    }
    // A live SQLite backup is consistent, but the blobs beside it and an active outbox
    // must stop changing as well. Never quit the old application on the user's behalf.
    if let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        for name in [
            "io.github.omarsimon1.Dispatchy",
            "io.github.omarsimon1.Dispatchy.Devel",
        ] {
            let owned = bus
                .call_sync(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "NameHasOwner",
                    Some(&(name,).to_variant()),
                    Some(glib::VariantTy::new("(b)").expect("valid variant type")),
                    gio::DBusCallFlags::NONE,
                    2000,
                    gio::Cancellable::NONE,
                )
                .map_err(std::io::Error::other)?;
            if owned.get::<(bool,)>().is_some_and(|(running,)| running) {
                return Err(std::io::Error::other(
                    "quit Dispatchy before the first Pronto launch, then retry",
                ));
            }
        }
    }
    migration::from_dispatchy(dirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a private D-Bus session under tools/headless.sh"]
    fn legacy_migration_guard() {
        assert_eq!(std::env::var("GDK_BACKEND").as_deref(), Ok("broadway"));
        let root = std::path::PathBuf::from(".local")
            .join(format!("migration-guard-{}", getrandom::u64().unwrap()));
        let dirs = sealwax_infrastructure::xdg::SealwaxDirs::under(&root);
        let old = dirs.config.with_file_name("dispatchy");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.toml"), "# fictional settings").unwrap();
        let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap();
        let name = "io.github.omarsimon1.Dispatchy";
        let request = bus
            .call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "RequestName",
                Some(&(name, 4u32).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                2000,
                gio::Cancellable::NONE,
            )
            .unwrap();
        assert_eq!(request.get::<(u32,)>(), Some((1,)));
        assert!(
            migrate_dirs(&dirs)
                .unwrap_err()
                .to_string()
                .contains("quit Dispatchy")
        );
        assert!(!dirs.config.exists());
        bus.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "ReleaseName",
            Some(&(name,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
        )
        .unwrap();
        migrate_dirs(&dirs).unwrap();
        assert_eq!(
            std::fs::read(dirs.config_file()).unwrap(),
            std::fs::read(old.join("config.toml")).unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
