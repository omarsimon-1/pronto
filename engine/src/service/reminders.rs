//! Remind Me's timer (the fork's Superhuman pass, docs/superhuman/remind.md): brings back the
//! reminders that are due through the store's wake, at the engine's start (overdue ones), at
//! the earliest wake time, when the app sets or cancels one (`RemindersChanged`) and at a
//! reconnect, which also follows a resume (a sleeping timer doesn't count a suspend). The wake
//! queues the moves back to the Inbox as ops due at once and this wakes the accounts' op
//! runners; an account offline makes them when it connects, as any queued op. Since phase C
//! new mail wakes it too: an answer to a conversation waiting in Reminders ends the reminder
//! and brings what waited back beside it (Superhuman's "if no reply"), and Someday reminders
//! are never waited for.

use super::{Shared, actor, wake};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The longest the timer sleeps while a reminder waits: a wall clock that moved on without a
/// reconnect (set by hand, a suspend the app didn't hear of) is caught within it.
const RECHECK: Duration = Duration::from_secs(60);

/// Unix seconds, the reminders' time.
fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// How long to sleep at `now` before the reminder due at `next`.
fn wait(now: i64, next: Option<i64>) -> Option<Duration> {
    next.map(|at| Duration::from_secs(u64::try_from(at - now).unwrap_or(0)).min(RECHECK))
}

/// Runs until the engine stops (`nudges` closes): wakes what is due, then sleeps until the next
/// is, or until a nudge.
pub(super) async fn run(shared: Arc<Shared>, nudges: async_channel::Receiver<()>) {
    loop {
        let now = unix_seconds();
        let next = match shared.store.wake_reminders(now).recv().await {
            Ok(woken) => {
                if !woken.woken.is_empty() {
                    tracing::info!(reminders = woken.woken.len(), "reminders came back");
                }
                if !woken.ended.is_empty() {
                    tracing::info!(reminders = woken.ended.len(), "reminders ended");
                }
                for account in woken.accounts {
                    wake(&shared, account, actor::Control::Ops);
                }
                woken.next
            }
            Err(error) => {
                tracing::warn!(%error, "due reminders couldn't be woken");
                Some(now + RECHECK.as_secs() as i64)
            }
        };
        let sleep = async {
            match wait(now, next) {
                Some(wait) => tokio::time::sleep(wait).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            nudge = nudges.recv() => {
                if nudge.is_err() {
                    return;
                }
                // A burst of nudges (mail arriving in batches) is one wake.
                while nudges.try_recv().is_ok() {}
            }
            () = sleep => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_timer_sleeps_until_the_next_reminder_and_never_long() {
        assert_eq!(wait(100, None), None);
        assert_eq!(wait(100, Some(130)), Some(Duration::from_secs(30)));
        // Due already (a wake time in the past): at once.
        assert_eq!(wait(100, Some(40)), Some(Duration::ZERO));
        // Far off: a look at the wall clock every minute meanwhile.
        assert_eq!(wait(100, Some(100 + 86_400)), Some(RECHECK));
    }
}
