use super::*;
use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};

pub fn present(this: &Rc<Controller>, window: &impl IsA<gtk::Widget>) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Settings");
    dialog.set_content_width(640);
    let page = adw::PreferencesPage::new();
    page.set_title("AI");
    page.set_icon_name(Some("document-edit-symbolic"));
    let group = adw::PreferencesGroup::new();
    group.set_title("Agent and model");
    group.set_description(Some(
        "Uses your signed-in command-line agent subscription. Pronto does not store API keys.",
    ));
    let agents = this.agents.borrow().clone();
    let mut labels = vec!["Omarchy default agent".to_owned()];
    labels.extend(agents.iter().map(|a| a.name.clone()));
    let strings = gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    let picker = adw::ComboRow::builder()
        .title("Agent")
        .model(&strings)
        .build();
    picker.set_widget_name("ai-agent");
    let options = this.options();
    picker.set_selected(
        agents
            .iter()
            .position(|a| a.name == options.agent)
            .map_or(0, |i| i as u32 + 1),
    );
    let model = adw::EntryRow::builder()
        .title("Model (blank uses the CLI default)")
        .text(&options.model)
        .build();
    model.set_widget_name("ai-model");
    let available_models = adw::ComboRow::builder()
        .title("Models listed by the CLI")
        .build();
    available_models.set_visible(false);
    let note = gtk::Label::new(None);
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.set_margin_top(8);
    note.set_margin_bottom(8);
    note.set_widget_name("ai-agent-status");
    group.add(&picker);
    group.add(&model);
    group.add(&available_models);
    group.add(&note);
    let features = adw::PreferencesGroup::new();
    features.set_title("Optional AI features");
    let style = adw::SwitchRow::builder()
        .title("Match my writing style")
        .subtitle("Learn from a sample of recent Sent emails. Message contents go to the chosen agent’s provider.")
        .active(options.style)
        .build();
    style.set_widget_name("ai-style");
    let auto = adw::SwitchRow::builder().title("Draft a reply to each new message").subtitle("Message contents are sent to the chosen agent’s provider. Drafts only; nothing is sent automatically.").active(options.auto).build();
    auto.set_widget_name("ai-auto");
    let cap = adw::SpinRow::with_range(1.0, 2000.0, 1.0);
    cap.set_title("Draft attempts per day");
    cap.set_value(options.daily_cap as f64);
    cap.set_widget_name("ai-cap");
    let unread = gtk::Button::with_label("Draft for unread in Inbox now");
    unread.set_widget_name("ai-unread");
    features.add(&style);
    features.add(&auto);
    features.add(&cap);
    features.add(&unread);
    let progress = gtk::Label::new(None);
    progress.set_wrap(true);
    progress.set_xalign(0.0);
    progress.set_margin_top(12);
    progress.set_widget_name("ai-progress");
    features.add(&progress);
    let cancel = gtk::Button::with_label("Pause AI work");
    cancel.set_widget_name("ai-cancel");
    features.add(&cancel);
    let weak = Rc::downgrade(this);
    cancel.connect_clicked(move |_| {
        if let Some(this) = weak.upgrade() {
            this.cancel.borrow().cancel();
            this.cancel.replace(Cancel::default());
            let paused = !this.saved.borrow().paused;
            this.saved.borrow_mut().paused = paused;
            this.persist();
            this.wake();
            this.status.replace(
                if paused {
                    "AI work paused; progress saved"
                } else {
                    "Resuming AI work"
                }
                .into(),
            );
        }
    });
    let weak = Rc::downgrade(this);
    unread.connect_clicked(move |_| {
        if let Some(this) = weak.upgrade() {
            this.unread_now();
        }
    });
    for (row, is_style) in [(style.clone(), true), (auto.clone(), false)] {
        let weak = Rc::downgrade(this);
        row.connect_active_notify(move |row| {
            if let Some(this) = weak.upgrade() {
                let mut options = this.options();
                if is_style {
                    options.style = row.is_active();
                } else {
                    options.auto = row.is_active();
                }
                this.set_options(options);
            }
        });
    }
    let weak = Rc::downgrade(this);
    cap.connect_value_notify(move |row| {
        if let Some(this) = weak.upgrade() {
            let mut options = this.options();
            options.daily_cap = row.value() as u32;
            this.set_options(options);
        }
    });
    let weak = Rc::downgrade(this);
    model.connect_changed(move |row| {
        if let Some(this) = weak.upgrade() {
            let mut options = this.options();
            options.model = row.text().to_string();
            this.set_options(options);
        }
    });
    let weak = Rc::downgrade(this);
    picker.connect_selected_notify(move |row| {
        if let Some(this) = weak.upgrade() {
            let mut options = this.options();
            let agent = row
                .selected()
                .checked_sub(1)
                .and_then(|i| this.agents.borrow().get(i as usize).map(|a| a.name.clone()))
                .unwrap_or_else(|| "default".into());
            // Filling the list after detection reselects the saved agent; that is no change.
            if agent == options.agent {
                return;
            }
            options.agent = agent;
            options.model.clear();
            this.set_options(options);
        }
    });
    let weak_model = model.downgrade();
    available_models.connect_selected_notify(move |row| {
        if let (Some(model), Some(item)) = (
            weak_model.upgrade(),
            row.selected_item().and_downcast::<gtk::StringObject>(),
        ) {
            model.set_text(&item.string());
        }
    });
    let sheets = adw::PreferencesGroup::new();
    sheets.set_title("Writing-style sheets");
    sheets.set_description(Some(
        "Learn from recent emails, keeping the style sheet and about 20 short examples. Sampled bodies are temporary. Refresh updates the sheet using only new mail; Rebuild starts a new sample.",
    ));
    if this.saved.borrow().legacy_style_cache {
        let legacy_cache = adw::ActionRow::builder()
            .title("Existing style sheets are kept")
            .subtitle("Earlier builds used the shared mail cache. Those downloads cannot be safely separated from other mail, so they follow [storage] cache_size in config.toml. New style samples do not add to that cache.")
            .build();
        legacy_cache.set_widget_name("ai-style-legacy-cache");
        sheets.add(&legacy_cache);
    }
    let style_messages = adw::SpinRow::with_range(1.0, MORE_STYLE_MESSAGES as f64, 1.0);
    style_messages.set_title("Emails to sample");
    style_messages.set_subtitle(&format!(
        "At most {DEFAULT_STYLE_MESSAGES} by default; unsuitable emails are skipped."
    ));
    style_messages.set_value(options.style_messages as f64);
    style_messages.set_widget_name("ai-style-messages");
    let style_months = adw::SpinRow::with_range(1.0, MORE_STYLE_MONTHS as f64, 1.0);
    style_months.set_title("Recent months to consider");
    style_months.set_subtitle(&format!(
        "The last {DEFAULT_STYLE_MONTHS} months by default."
    ));
    style_months.set_value(options.style_months as f64);
    style_months.set_widget_name("ai-style-months");
    let style_refresh_messages =
        adw::SpinRow::with_range(1.0, DEFAULT_STYLE_REFRESH_MESSAGES as f64, 1.0);
    style_refresh_messages.set_title("New emails per refresh");
    style_refresh_messages.set_subtitle(&format!(
        "At most {DEFAULT_STYLE_REFRESH_MESSAGES} by default, since this account’s last run."
    ));
    style_refresh_messages.set_value(options.style_refresh_messages as f64);
    style_refresh_messages.set_widget_name("ai-style-refresh-messages");
    let more = gtk::Button::with_label("Use more emails");
    more.set_tooltip_text(Some(&format!(
        "Sample up to {MORE_STYLE_MESSAGES} emails from the last {MORE_STYLE_MONTHS} months on the next rebuild."
    )));
    more.set_widget_name("ai-style-more");
    sheets.add(&style_messages);
    sheets.add(&style_months);
    sheets.add(&style_refresh_messages);
    sheets.add(&more);
    for (row, field) in [
        (style_messages.clone(), 0),
        (style_months.clone(), 1),
        (style_refresh_messages.clone(), 2),
    ] {
        let weak = Rc::downgrade(this);
        row.connect_value_notify(move |row| {
            if let Some(this) = weak.upgrade() {
                let mut options = this.options();
                let setting = match field {
                    0 => &mut options.style_messages,
                    1 => &mut options.style_months,
                    _ => &mut options.style_refresh_messages,
                };
                let value = row.value() as u32;
                if *setting != value {
                    *setting = value;
                    this.set_options(options);
                }
            }
        });
    }
    let weak = Rc::downgrade(this);
    let messages = style_messages.downgrade();
    let months = style_months.downgrade();
    more.connect_clicked(move |_| {
        if let Some(this) = weak.upgrade() {
            let mut options = this.options();
            options.style_messages = MORE_STYLE_MESSAGES;
            options.style_months = MORE_STYLE_MONTHS;
            this.set_options(options);
            if let (Some(messages), Some(months)) = (messages.upgrade(), months.upgrade()) {
                messages.set_value(MORE_STYLE_MESSAGES as f64);
                months.set_value(MORE_STYLE_MONTHS as f64);
            }
        }
    });
    let mut dates = Vec::new();
    if let Some(state) = this.state.upgrade() {
        for account in state.mailbox().accounts() {
            let row = adw::ActionRow::builder().title(&account.name).build();
            let edit = gtk::Button::with_label("View/Edit style sheet");
            edit.set_valign(gtk::Align::Center);
            let rebuild = gtk::Button::with_label("Rebuild");
            rebuild.set_valign(gtk::Align::Center);
            rebuild.set_tooltip_text(Some(
                "Replace the sheet with a new sample using the current limits.",
            ));
            let refresh = gtk::Button::with_label("Refresh");
            refresh.set_valign(gtk::Align::Center);
            refresh.set_tooltip_text(Some(
                "Update the existing sheet using only newly sent emails.",
            ));
            row.add_suffix(&edit);
            row.add_suffix(&refresh);
            row.add_suffix(&rebuild);
            sheets.add(&row);
            let path = this.style_path(&account);
            let parent = window
                .clone()
                .upcast::<gtk::Widget>()
                .root()
                .and_downcast::<gtk::Window>();
            let weak = Rc::downgrade(this);
            edit.connect_clicked(move |_| {
                let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
                let parent = parent.clone();
                let weak = weak.clone();
                glib::spawn_future_local(async move {
                    if launcher.launch_future(parent.as_ref()).await.is_err()
                        && let Some(this) = weak.upgrade()
                    {
                        this.status.replace(
                            "The style sheet is not built yet, or no Markdown editor is available"
                                .into(),
                        );
                    }
                });
            });
            let weak = Rc::downgrade(this);
            let id = account.id;
            rebuild.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.rebuild(id);
                }
            });
            let weak = Rc::downgrade(this);
            refresh.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.refresh(id);
                }
            });
            dates.push((account.id, row, rebuild, refresh));
        }
    }
    let advanced = adw::PreferencesGroup::new();
    let config = gtk::Button::with_label("Edit other settings…");
    config.set_widget_name("settings-config");
    advanced.add(&config);
    let weak = Rc::downgrade(this);
    config.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade().and_then(|a| a.state.upgrade())
            && let (Some(window), Some(prefs)) = (state.window(), state.preferences())
        {
            let path = prefs.path(crate::preferences::SettingsFile::Config);
            glib::spawn_future_local(async move {
                let message = crate::preferences::open_in_editor(
                    window.window(),
                    path,
                    crate::preferences::SettingsFile::Config,
                )
                .await;
                window.add_toast(adw::Toast::new(&message));
            });
        }
    });
    page.add(&group);
    page.add(&features);
    page.add(&sheets);
    page.add(&advanced);
    dialog.add(&page);
    let weak = Rc::downgrade(this);
    let weak_dialog = dialog.downgrade();
    let last_agent = Rc::new(RefCell::new(String::new()));
    // The CLIs are first probed when this dialog opens, so the list may fill in a moment later.
    let listed = Cell::new(this.detected.get());
    let update = move || {
        let (Some(this), Some(dialog)) = (weak.upgrade(), weak_dialog.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        if !dialog.is_visible() {
            return glib::ControlFlow::Break;
        }
        let options = this.options();
        if !listed.get() && this.detected.get() {
            listed.set(true);
            let names = this
                .agents
                .borrow()
                .iter()
                .map(|a| a.name.clone())
                .collect::<Vec<_>>();
            strings.splice(
                1,
                strings.n_items() - 1,
                &names.iter().map(String::as_str).collect::<Vec<_>>(),
            );
            picker.set_selected(
                names
                    .iter()
                    .position(|name| *name == options.agent)
                    .map_or(0, |i| i as u32 + 1),
            );
            // Read the detected agent's models below.
            last_agent.borrow_mut().clear();
        }
        let agent = this.selected_agent();
        let enabled = this.loaded.get() && agent.as_ref().is_some_and(|a| a.available);
        style.set_sensitive(enabled);
        auto.set_sensitive(enabled);
        cap.set_sensitive(enabled);
        style_messages.set_sensitive(enabled);
        style_months.set_sensitive(enabled);
        style_refresh_messages.set_sensitive(enabled);
        more.set_sensitive(enabled);
        unread.set_sensitive(enabled && options.auto);
        picker.set_sensitive(this.loaded.get() && this.detected.get());
        model.set_sensitive(agent.as_ref().is_some_and(|a| a.model));
        let status = if !this.detected.get() {
            "Detecting signed-in agent CLIs…".into()
        } else if this.agents.borrow().is_empty() {
            "AI features need a signed-in agent CLI".into()
        } else if let Some(agent) = &agent {
            format!("{} · {}", agent.name, agent.reason)
        } else {
            "The chosen CLI has no verified no-tools invocation. Choose another installed agent."
                .into()
        };
        note.set_text(&status);
        progress.set_text(&this.status.borrow());
        cancel.set_label(if this.saved.borrow().paused {
            "Resume AI work"
        } else {
            "Pause AI work"
        });
        for (id, row, rebuild, refresh) in &dates {
            rebuild.set_sensitive(enabled && options.style);
            let built = this.saved.borrow().styles.get(&id.0).map_or(0, |s| s.built);
            refresh.set_sensitive(enabled && options.style && built != 0);
            row.set_subtitle(&if built == 0 {
                "Not built yet".into()
            } else {
                glib::DateTime::from_unix_local(built / 1000)
                    .ok()
                    .and_then(|d| d.format("Last updated %Y-%m-%d %H:%M").ok())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Built".into())
            });
        }
        let key = options.agent.clone();
        if *last_agent.borrow() != key {
            last_agent.replace(key.clone());
            model.set_text(&options.model);
            available_models.set_visible(false);
            if let Some(agent) = agent {
                let row = available_models.downgrade();
                let current = last_agent.clone();
                glib::spawn_future_local(async move {
                    let models = gio::spawn_blocking(move || agent.list_models(&Cancel::default()))
                        .await
                        .unwrap_or_default();
                    if *current.borrow() == key
                        && !models.is_empty()
                        && let Some(row) = row.upgrade()
                    {
                        let _freeze = row.freeze_notify();
                        row.set_model(Some(&gtk::StringList::new(
                            &models.iter().map(String::as_str).collect::<Vec<_>>(),
                        )));
                        row.set_selected(gtk::INVALID_LIST_POSITION);
                        row.set_visible(true);
                    }
                });
            }
        }
        glib::ControlFlow::Continue
    };
    dialog.present(Some(window));
    update();
    glib::timeout_add_local(Duration::from_millis(300), update);
}
