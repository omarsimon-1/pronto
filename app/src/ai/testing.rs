//! Hooks used only by the explicit --self-test runner, over synthetic mail.
use super::*;
impl Controller {
    pub(crate) async fn test_agent(&self, path: PathBuf) -> Result<(), String> {
        for _ in 0..300 {
            if self.loaded.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        if !self.loaded.get() {
            return Err("AI controller did not load".into());
        }
        self.cancel.borrow().cancel();
        self.cancel.replace(Cancel::default());
        self.agents.replace(vec![Agent::test_fixture(path)]);
        self.default_agent.replace(Some("fixture".into()));
        self.detected.set(true);
        let mut options = self.options();
        options.agent = "fixture".into();
        self.set_options(options);
        Ok(())
    }
    pub(crate) fn test_event(&self, account: AccountId, folder: FolderId, message: MessageId) {
        self.event(&Event::NewMail {
            account,
            folder,
            messages: vec![message],
        });
    }
    pub(crate) fn test_count(&self, field: &str) -> Result<usize, String> {
        let saved = self.saved.borrow();
        match field {
            "ready" => Ok(saved.ready.len()),
            "queue" => Ok(saved.queue.len()),
            "used" => Ok(saved.used as usize),
            "built" => Ok(saved.styles.values().filter(|s| s.built > 0).count()),
            "style-pending" => Ok(saved
                .styles
                .values()
                .filter(|s| {
                    s.built == 0 || s.prepared || s.refresh_requested || s.rebuild_requested
                })
                .count()),
            "sampled" => Ok(saved.styles.values().map(|s| s.processed).sum()),
            "batches" => Ok(saved.styles.values().map(|s| s.summaries.len()).sum()),
            _ => Err("Unknown AI count".into()),
        }
    }
    pub(crate) async fn test_resume(&self) -> Result<(), String> {
        self.cancel.borrow().cancel();
        self.cancel.replace(Cancel::default());
        let paused = self.saved.borrow().paused;
        self.saved.borrow_mut().paused = true;
        while self.in_flight.get() {
            glib::timeout_future(Duration::from_millis(25)).await;
        }
        self.style_examples.borrow_mut().clear();
        self.saved.borrow_mut().paused = paused;
        self.checkpoint().await?;
        let path = self
            .state
            .upgrade()
            .ok_or("App closed")?
            .config()
            .dirs
            .state
            .join("ai.json");
        let saved = gio::spawn_blocking(move || {
            std::fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
        })
        .await
        .map_err(|_| "Read failed")?
        .ok_or("Checkpoint missing")?;
        self.saved.replace(saved);
        self.wake();
        Ok(())
    }
    pub(crate) async fn test_retry_now(&self) {
        for _ in 0..100 {
            if self.saved.borrow().queue.iter().any(|j| j.retry_at > 0) {
                break;
            }
            glib::timeout_future(Duration::from_millis(25)).await;
        }
        for job in &mut self.saved.borrow_mut().queue {
            job.retry_at = 0;
        }
        self.held_until.set(0);
        self.wake();
    }
}
