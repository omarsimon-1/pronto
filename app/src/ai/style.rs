//! Bounded style learning. Mail exists only in memory; checkpoints contain summaries and marks.
use super::*;
use sealwax_engine::sent_sample::{
    Cancel as SampleCancel, FolderCheckpoint, SampleOptions, SentSample,
};

pub(super) const DEFAULT_STYLE_MESSAGES: u32 = 300;
pub(super) const DEFAULT_STYLE_MONTHS: u32 = 12;
pub(super) const DEFAULT_STYLE_REFRESH_MESSAGES: u32 = 50;
pub(super) const MORE_STYLE_MESSAGES: u32 = 1000;
pub(super) const MORE_STYLE_MONTHS: u32 = 24;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Style {
    pub(super) summaries: Vec<String>,
    pub(super) prepared: bool,
    pub(super) built: i64,
    pub(super) retry_at: i64,
    pub(super) last_processed_date: i64,
    pub(super) checkpoints: Vec<FolderCheckpoint>,
    pending_checkpoints: Vec<FolderCheckpoint>,
    pending_date: i64,
    pub(super) refresh_requested: bool,
    pub(super) rebuild_requested: bool,
    pub(super) processed: usize,
    pub(super) sampled: usize,
}

impl Saved {
    pub(super) fn migrate_style_sampling(&mut self) {
        if self.style_sampling_version >= 1 {
            return;
        }
        // Old style downloads shared the normal mail cache; their provenance was not recorded.
        // Never delete shared blobs or the user's sheet. Settings explains normal cache eviction.
        self.legacy_style_cache = !self.styles.is_empty();
        for style in self.styles.values_mut() {
            style.summaries.clear();
            style.prepared = false;
            style.last_processed_date = style.built / 1000;
        }
        self.style_sampling_version = 1;
    }
}

fn window_start(now: i64, months: u32) -> i64 {
    glib::DateTime::from_unix_utc(now)
        .and_then(|date| date.add_months(-(months.clamp(1, MORE_STYLE_MONTHS) as i32)))
        .map(|date| date.to_unix())
        .unwrap_or(now - i64::from(months.clamp(1, MORE_STYLE_MONTHS)) * 31 * 86_400)
}

impl Controller {
    pub fn refresh(&self, account: AccountId) {
        self.cancel.borrow().cancel();
        self.cancel.replace(Cancel::default());
        let mut saved = self.saved.borrow_mut();
        let style = saved.styles.entry(account.0).or_default();
        style.refresh_requested = true;
        style.retry_at = 0;
        drop(saved);
        self.persist();
        self.wake();
    }

    pub fn rebuild(&self, account: AccountId) {
        self.cancel.borrow().cancel();
        self.cancel.replace(Cancel::default());
        self.style_examples.borrow_mut().remove(&account.0);
        let mut saved = self.saved.borrow_mut();
        let style = saved.styles.entry(account.0).or_default();
        // Keep the old sheet usable until its replacement has been written successfully.
        *style = Style {
            built: style.built,
            rebuild_requested: true,
            ..Style::default()
        };
        drop(saved);
        self.persist();
        self.wake();
    }

    async fn sample_sent(
        &self,
        state: &AppState,
        store: &Store,
        account: &Account,
        options: SampleOptions,
        cancel: &Cancel,
    ) -> Result<SentSample, String> {
        let local = account.key.is_none()
            || state.preferences().is_some_and(|preferences| {
                preferences.accounts().iter().any(|settings| {
                    account.key.as_ref() == Some(&settings.key)
                        && matches!(settings.receive, sealwax_domain::account::Receive::Pop3(_))
                })
            });
        if local {
            return self
                .local_style_sample(state, store, account, options, cancel)
                .await;
        }
        let request = self.style_request.get().wrapping_add(1);
        self.style_request.set(request);
        let token = SampleCancel::default();
        let (tx, rx) = async_channel::unbounded();
        let _subscription = state.engine().subscribe(move |event| match event {
            Event::SentSampled {
                request: id,
                result,
                ..
            } if *id == request => {
                let _ = tx.try_send(Ok(result.clone()));
            }
            Event::SentSampleProgress {
                request: id,
                fetched,
                total,
                ..
            } if *id == request => {
                let _ = tx.try_send(Err((*fetched, *total)));
            }
            _ => {}
        });
        state
            .engine()
            .send(Command::SampleSent {
                request,
                account: account.id,
                options,
                cancel: token.clone(),
            })
            .map_err(|_| "Waiting for Sent mail connection")?;
        // A slow server may take several minutes; cancellation is still observed every 100 ms.
        for _ in 0..6000 {
            if cancel.cancelled() {
                token.cancel();
                return Err("Style learning paused".into());
            }
            while let Ok(event) = rx.try_recv() {
                match event {
                    Ok(result) => {
                        return result.map_err(|_| "Cannot sample Sent mail; will retry".into());
                    }
                    Err((fetched, total)) => {
                        self.status.replace(format!("{}: Learning from {total} recent emails · {fetched}/{total} downloaded", account.name));
                    }
                }
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        token.cancel();
        Err("Sent sampling timed out; will retry".into())
    }

    pub(super) async fn style_step(
        &self,
        state: &AppState,
        store: &Store,
        agent: &Agent,
        account: &Account,
        mut style: Style,
        cancel: &Cancel,
    ) -> Result<(), String> {
        // If the app exited during a batch, finish from the anonymized summaries already saved.
        // With no completed batch there is no useful checkpoint, so start a fresh bounded sample.
        if style.prepared
            && style.summaries.is_empty()
            && !self.style_examples.borrow().contains_key(&account.id.0)
        {
            style.prepared = false;
        }
        let updating = style.built > 0 && !style.rebuild_requested;
        if !style.prepared {
            let options = self.options();
            let sampled_at = now_ms() / 1000;
            let limit = if updating {
                options
                    .style_refresh_messages
                    .clamp(1, DEFAULT_STYLE_REFRESH_MESSAGES)
            } else {
                options.style_messages.clamp(1, MORE_STYLE_MESSAGES)
            } as usize;
            let since = window_start(sampled_at, options.style_months);
            self.status.replace(format!(
                "{}: Selecting up to {limit} recent emails · headers only",
                account.name
            ));
            let identities = senders(
                account,
                &state
                    .preferences()
                    .map(|p| p.accounts())
                    .unwrap_or_default(),
            );
            let sample = self
                .sample_sent(
                    state,
                    store,
                    account,
                    SampleOptions {
                        since: if updating {
                            since.max(style.last_processed_date)
                        } else {
                            since
                        },
                        limit,
                        refresh: updating,
                        checkpoints: if updating {
                            style.checkpoints.clone()
                        } else {
                            Vec::new()
                        },
                        own_addresses: identities.iter().map(|i| i.address.clone()).collect(),
                    },
                    cancel,
                )
                .await?;
            if cancel.cancelled() {
                return Ok(());
            }
            let signatures = identities
                .into_iter()
                .filter_map(|i| i.signature)
                .collect::<Vec<_>>();
            let originals = sample.originals;
            let examples = gio::spawn_blocking(move || {
                originals
                    .into_iter()
                    .filter_map(|raw| text::style_message(&raw.0, &signatures))
                    .collect::<std::collections::VecDeque<_>>()
            })
            .await
            .map_err(|_| "Cannot prepare style examples")?;
            if cancel.cancelled() {
                return Ok(());
            }
            style.sampled = sample.fetched;
            style.processed = 0;
            style.pending_checkpoints = sample.checkpoints;
            style.pending_date = sampled_at;
            style.prepared = true;
            self.style_examples
                .borrow_mut()
                .insert(account.id.0, examples);
            self.saved
                .borrow_mut()
                .styles
                .insert(account.id.0, style.clone());
            self.checkpoint().await?;
        }
        if cancel.cancelled() {
            return Ok(());
        }
        let mut batch = String::new();
        let mut count = 0;
        if let Some(examples) = self.style_examples.borrow().get(&account.id.0) {
            for example in examples.iter().take(20) {
                if !batch.is_empty() && batch.len() + example.len() > 28_000 {
                    break;
                }
                batch.push_str(example);
                count += 1;
            }
        }
        if count > 0 {
            self.status.replace(format!(
                "{}: Learning from {} recent emails · {} examples processed",
                account.name, style.sampled, style.processed
            ));
            let summary = self
                .generate(agent, text::style_prompt(&batch, false), cancel)
                .await?;
            if cancel.cancelled() {
                return Ok(());
            }
            style.summaries.push(summary.chars().take(8000).collect());
            style.processed += count;
            if let Some(examples) = self.style_examples.borrow_mut().get_mut(&account.id.0) {
                examples.drain(..count);
            }
        } else if style.summaries.iter().map(String::len).sum::<usize>() > 28_000 {
            let count = style.summaries.len().min(5);
            let summary = self
                .generate(
                    agent,
                    text::style_prompt(&style.summaries[..count].join("\n\n"), true),
                    cancel,
                )
                .await?;
            style.summaries.drain(..count);
            style.summaries.push(summary.chars().take(8000).collect());
        } else {
            if !style.summaries.is_empty() {
                let input = style.summaries.join("\n\n");
                let prompt = if updating {
                    text::style_update_prompt(&self.style_reference(account).await?, &input)
                } else {
                    text::style_prompt(&input, true)
                };
                let guide = self.generate(agent, prompt, cancel).await?;
                if cancel.cancelled() {
                    return Ok(());
                }
                let path = self.style_path(account);
                gio::spawn_blocking(move || save_sheet(&path, &guide))
                    .await
                    .map_err(|_| "Cannot save style sheet")??;
                style.built = now_ms();
            } else if style.built == 0 {
                // Do not invent a style sheet from empty or machine-generated mail.
                style.prepared = false;
                style.retry_at = now_ms() + 3_600_000;
                self.style_examples.borrow_mut().remove(&account.id.0);
                self.saved.borrow_mut().styles.insert(account.id.0, style);
                self.checkpoint().await?;
                self.status.replace(format!(
                    "{}: No suitable recent writing yet; will retry",
                    account.name
                ));
                return Ok(());
            }
            style.last_processed_date = style.pending_date;
            style.checkpoints = std::mem::take(&mut style.pending_checkpoints);
            style.pending_date = 0;
            style.summaries.clear();
            style.prepared = false;
            style.refresh_requested = false;
            style.rebuild_requested = false;
            style.retry_at = 0;
            self.style_examples.borrow_mut().remove(&account.id.0);
            self.status.replace(format!(
                "{}: Writing-style sheet ready · {} new examples",
                account.name, style.processed
            ));
        }
        if !cancel.cancelled() {
            self.saved.borrow_mut().styles.insert(account.id.0, style);
            self.checkpoint().await?;
        }
        Ok(())
    }
}

fn save_sheet(path: &std::path::Path, guide: &str) -> Result<(), String> {
    sealwax_infrastructure::xdg::ensure_private_dir(path.parent().ok_or("Missing style directory")?)
        .and_then(|_| sealwax_infrastructure::accounts::replace(path, guide))
        .map_err(|_| "Cannot save style sheet".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_progress_does_not_resume_full_download_or_discard_sheet_date() {
        let mut saved: Saved = serde_json::from_str(r#"{"styles":{"1":{"ids":[1,2,3],"cursor":1,"summaries":["legacy"],"prepared":true,"built":123000}}}"#).unwrap();
        saved.migrate_style_sampling();
        let style = &saved.styles[&1];
        assert_eq!(style.built, 123000);
        assert_eq!(style.last_processed_date, 123);
        assert!(!style.prepared);
        assert!(style.summaries.is_empty());
        assert!(saved.legacy_style_cache);
        let json = serde_json::to_string(&saved).unwrap();
        assert!(!json.contains("legacy\""));
        assert!(!json.contains("\"ids\""));
        saved.migrate_style_sampling();
        assert_eq!(saved.styles[&1].built, 123000);
    }
    #[test]
    fn old_options_get_bounded_sampling_defaults() {
        let options: Options = serde_json::from_str(r#"{"style":true}"#).unwrap();
        assert_eq!(options.style_messages, 300);
        assert_eq!(options.style_months, 12);
        assert_eq!(options.style_refresh_messages, 50);
    }
    #[test]
    fn window_uses_calendar_months() {
        let now = glib::DateTime::from_utc(2026, 10, 30, 0, 0, 0.0).unwrap();
        let start = glib::DateTime::from_unix_utc(window_start(now.to_unix(), 12)).unwrap();
        assert_eq!(start.format("%Y-%m-%d").unwrap().as_str(), "2025-10-30");
    }
    #[test]
    fn saving_sheet_leaves_only_markdown_and_no_temporary_body_cache() {
        let dir = std::env::temp_dir().join(format!("pronto-style-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.md");
        save_sheet(&path, "# Writing style\nUse short sentences.\n").unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("# Writing style")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
