//! The op runner (ML-051, ADR-015): the account's due ops from the store, run on the command
//! connection before the passes, one group (one folder, one action, a UID set) at a time. A
//! group holds its folders' pass locks from before its first command until its outcome is
//! recorded, so a pass on the IDLE connection never commits what it read before the change
//! after the record. A move takes the target's mark before its first attempt, and every attempt
//! is counted in the store before it starts: a cut connection, `Reconnect`, a stop or a crash
//! leaves the group to run again as a retry, which finds what the server already did. The
//! server's refusals, and those the sync paths made, reach the app as `Event::OpsRefused`.

use super::{Interrupt, Poller, backoff_delay, discard, race};
use crate::api::{Event, FolderError, Refused, SyncStatus};
use crate::imap::write::{
    self, Copy, Done, Mark, Move, Moving, OpFolder, WriteError, WriteFailure,
};
use sealwax_domain::model::FolderId;
use sealwax_domain::sync::{Facts, place_after_move};
use sealwax_infrastructure::store::{
    Failure, OpAction, OpGroup, OpId, OpsDone, OpsRefusal, RefusedChange, SyncState,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Attempts after which a group that keeps failing is refused: a NO without a code is retried,
/// and some servers might never stop saying it. Not while the mailbox is full.
pub(super) const MAX_ATTEMPTS: u32 = 5;
/// Rounds of due ops per wake: a re-targeted folder's ops come back due after its pass.
const ROUNDS: usize = 3;

/// Unix milliseconds, the store's time.
pub(super) fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// The longest a timer sleeps toward a store time (a send scheduled for later, an op's or a
/// copy's retry). Store times are the wall clock's, and a sleeping timer doesn't count a
/// suspend: a laptop that slept past the time would send hours late. The actor wakes within
/// this, asks the store what is due by the wall clock, and sleeps again (as Remind Me's
/// timer does, `reminders::RECHECK`).
pub(super) const RECHECK: Duration = Duration::from_secs(60);

/// How long to sleep at store time `now` before looking at what is due at `at`.
pub(super) fn wait_for(at: i64, now: i64) -> Duration {
    Duration::from_millis(u64::try_from(at.saturating_sub(now)).unwrap_or(0)).min(RECHECK)
}

/// The instant to look again for what a store time makes due, `now` being the store time of
/// `Instant::now()`: the time itself when it is near, else [`RECHECK`] from now.
pub(super) fn instant_at(at: i64, now: i64) -> Instant {
    Instant::now() + wait_for(at, now)
}

/// The app's name for a refused change.
pub(super) fn refused(change: RefusedChange) -> Refused {
    match change {
        RefusedChange::Flags => Refused::Flags,
        RefusedChange::Move => Refused::Move,
        RefusedChange::Expunge => Refused::Expunge,
        RefusedChange::FolderRead => Refused::FolderRead,
        RefusedChange::Copy => Refused::Copy,
    }
}

fn kind(action: &OpAction) -> &'static str {
    match action {
        OpAction::Flags { .. } => "flags",
        OpAction::Move { .. } => "move",
        OpAction::Expunge => "expunge",
        OpAction::ReadBelow { .. } => "folder read",
        OpAction::Copy { .. } => "copy",
    }
}

/// How a group ended.
enum Ran {
    /// Its outcome is recorded: done, or failed.
    Recorded,
    /// The connection broke: nothing recorded, the group runs again as a retry.
    Lost,
    /// Its folder's UIDVALIDITY isn't the ops': a pass resets the folder and the store finds
    /// the messages again by Message-ID.
    Resync(FolderId),
    /// Done, but the store couldn't place the messages in this folder (a move without COPYUID,
    /// into a folder never synced, or a copy): its pass fetches them now, so they show there,
    /// counted, at once rather than at the account pass. Never Gmail's All Mail while it was
    /// never synced ([`Poller::places`]).
    Place(FolderId),
}

/// A write's outcome, or what stopped it before the server was asked.
enum Outcome {
    Written(Result<Done, WriteError>),
    /// The store couldn't count the attempt: nothing was sent.
    Unstarted,
}

impl Poller {
    /// Runs the ops due now, group by group, and sets the timer for the next. A lost
    /// connection ends the round like a failed pass; an interrupt ends it at once. Either way
    /// the ops already started run again as retries.
    pub(super) async fn run_ops(&mut self, seen: u64) -> Result<(), Interrupt> {
        self.ops_now = false;
        for _ in 0..ROUNDS {
            let now = unix_ms();
            self.prepare_folders(now, seen).await?;
            if self.conn.is_none() {
                return Ok(());
            }
            let due = match self.shared.store.due_ops(self.account, now).recv().await {
                Ok(due) => due,
                Err(error) => {
                    tracing::warn!(account = self.account.0, %error, "due ops couldn't be read");
                    self.next_ops = Some(Instant::now() + self.config().backoff_max);
                    return Ok(());
                }
            };
            self.refusals(&due.refused);
            self.next_ops = due.next.map(|at| instant_at(at, now));
            if due.groups.is_empty() {
                self.settle_quota().await;
                return Ok(());
            }
            let mut resync = Vec::new();
            for group in due.groups {
                match self.run_group(group, seen).await? {
                    Ran::Recorded => {}
                    Ran::Lost => {
                        self.lost();
                        return Ok(());
                    }
                    Ran::Resync(folder) | Ran::Place(folder) => resync.push(folder),
                }
            }
            resync.sort_unstable();
            resync.dedup();
            for folder in resync {
                self.folder_pass(folder, seen).await?;
                if self.conn.is_none() {
                    return Ok(());
                }
            }
        }
        self.next_ops = Some(Instant::now() + self.config().backoff_min);
        Ok(())
    }

    /// Prepares the Reminders folders that moves due at `now` go into (the fork's Remind Me,
    /// docs/superhuman/remind.md), before those moves run: one the store made is created on
    /// the server (CREATE, then SUBSCRIBE: `imap::manage::create_reminders`), and one the store
    /// never synced gets its first pass, so the moves land in rows of their own (COPYUID) and
    /// the conversation never leaves the store on its way. A refused folder goes from the
    /// store, which refuses the moves into it; the next read of the due ops reports them.
    async fn prepare_folders(&mut self, now: i64, seen: u64) -> Result<(), Interrupt> {
        let wanted = match self
            .shared
            .store
            .folders_to_prepare(self.account, now)
            .recv()
            .await
        {
            Ok(wanted) => wanted,
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "Reminders folders couldn't be read");
                return Ok(());
            }
        };
        for folder in wanted {
            if folder.create {
                let Some(conn) = self.conn.as_mut() else {
                    return Ok(());
                };
                let made = race(
                    &self.signals,
                    seen,
                    crate::imap::manage::create_reminders(conn, &folder.name_raw),
                )
                .await?;
                self.used = Instant::now();
                let store = &self.shared.store;
                let recorded = match made {
                    Ok(()) => {
                        tracing::info!(
                            account = self.account.0,
                            "the Reminders folder was created"
                        );
                        store.folder_created(folder.folder).recv().await.map(drop)
                    }
                    Err(FolderError::Lost) => {
                        self.lost();
                        return Ok(());
                    }
                    Err(error) => {
                        tracing::info!(
                            account = self.account.0,
                            %error,
                            "the server refused to create the Reminders folder"
                        );
                        let refused = store.folder_refused(folder.folder).recv().await;
                        if let Err(error) = refused {
                            tracing::warn!(account = self.account.0, %error, "a refused Reminders folder couldn't be recorded");
                        }
                        continue;
                    }
                };
                if let Err(error) = recorded {
                    tracing::warn!(account = self.account.0, %error, "a created Reminders folder couldn't be recorded");
                }
            }
            if folder.sync {
                self.folder_pass(folder.folder, seen).await?;
                if self.conn.is_none() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn refusals(&self, refusals: &[OpsRefusal]) {
        for refusal in refusals {
            tracing::info!(
                account = self.account.0,
                messages = refusal.messages,
                "changes refused"
            );
            self.shared.send(Event::OpsRefused {
                account: refusal.account,
                change: refused(refusal.change),
                messages: refusal.messages,
            });
        }
    }

    /// The mailbox no longer counts as full once nothing waits for the server.
    async fn settle_quota(&mut self) {
        if !self.quota {
            return;
        }
        let pending = self.shared.store.pending_ops(self.account).recv().await;
        if pending.is_ok_and(|pending| pending == 0) {
            self.quota = false;
            self.status.set(self.healthy());
        }
    }

    async fn run_group(&mut self, group: OpGroup, seen: u64) -> Result<Ran, Interrupt> {
        let started = Instant::now();
        if group.attempts >= MAX_ATTEMPTS && !self.quota {
            tracing::info!(
                account = self.account.0,
                op = kind(&group.action),
                attempts = group.attempts,
                "an op group is given up"
            );
            self.record_failure(&group, Failure::Permanent).await;
            return Ok(Ran::Recorded);
        }
        let Some(uidvalidity) = group.uidvalidity else {
            return Ok(Ran::Resync(group.folder));
        };
        let mut folders = vec![group.folder];
        if let OpAction::Move { to, .. } | OpAction::Copy { to, .. } = &group.action {
            folders.push(*to);
        }
        folders.sort_unstable();
        folders.dedup();
        let mut held = Vec::with_capacity(folders.len());
        for folder in folders {
            held.push(race(&self.signals, seen, self.locks.lock(folder)).await?);
        }
        // A copy's arrival in its target isn't new mail either; it settles when the target's
        // pass stores it.
        let target = match &group.action {
            OpAction::Move { to, .. } | OpAction::Copy { to, .. } => Some(*to),
            _ => None,
        };
        if let Some(to) = target {
            self.moved.start(
                to,
                group
                    .messages
                    .iter()
                    .filter_map(|message| message.message_id.clone()),
            );
        }
        let outcome = self.attempt(&group, uidvalidity, seen).await?;
        let ran = match outcome {
            Outcome::Unstarted => Ran::Recorded,
            Outcome::Written(Ok(done)) => {
                if let Some(to) = target {
                    let rehomed = group
                        .messages
                        .iter()
                        .filter(|message| done.moved.iter().any(|(old, _)| *old == message.uid));
                    self.moved.settled(
                        to,
                        rehomed.filter_map(|message| message.message_id.as_deref()),
                    );
                }
                let unplaced = self.record_done(&group, done).await;
                let place = target.filter(|to| {
                    matches!(group.action, OpAction::Copy { .. }) || unplaced.contains(to)
                });
                match place {
                    Some(to) if self.places(to).await => Ran::Place(to),
                    _ => Ran::Recorded,
                }
            }
            Outcome::Written(Err(error)) if error.is_connection() => {
                tracing::info!(
                    account = self.account.0,
                    op = kind(&group.action),
                    %error,
                    "the connection was lost during an op group"
                );
                Ran::Lost
            }
            Outcome::Written(Err(error)) => self.group_failed(&group, &error).await,
        };
        drop(held);
        tracing::debug!(
            account = self.account.0,
            op = kind(&group.action),
            messages = group.messages.len(),
            attempts = group.attempts + 1,
            retry = group.retry,
            ms = started.elapsed().as_secs_f64() * 1000.0,
            "op group ran"
        );
        Ok(ran)
    }

    /// The mark (a move's first attempt), the attempt counted in the store, then the write.
    async fn attempt(
        &mut self,
        group: &OpGroup,
        uidvalidity: u32,
        seen: u64,
    ) -> Result<Outcome, Interrupt> {
        let Some(conn) = self.conn.as_mut() else {
            return Ok(Outcome::Written(Err(WriteError::Imap(
                crate::imap::ImapError::ConnectionLost,
            ))));
        };
        let mark = match &group.action {
            OpAction::Move {
                to_name,
                to_uidvalidity,
                ..
            }
            | OpAction::Copy {
                to_name,
                to_uidvalidity,
                ..
            } => match group.target_uidnext {
                Some(uidnext) => Some(Mark {
                    uidvalidity: *to_uidvalidity,
                    uidnext,
                }),
                None => match race(&self.signals, seen, write::mark_target(conn, to_name)).await? {
                    Ok(mark) => Some(mark),
                    Err(error) => return Ok(Outcome::Written(Err(error))),
                },
            },
            _ => None,
        };
        let counted = self
            .shared
            .store
            .start_ops(group.ops.clone(), mark.map(|mark| mark.uidnext))
            .recv()
            .await;
        if let Err(error) = counted {
            tracing::warn!(account = self.account.0, %error, "an op group couldn't be started");
            return Ok(Outcome::Unstarted);
        }
        let folder = OpFolder {
            name_raw: &group.folder_name,
            uidvalidity,
        };
        let uids: Vec<u32> = group.messages.iter().map(|message| message.uid).collect();
        let written = match &group.action {
            OpAction::Flags { add, remove } => {
                race(
                    &self.signals,
                    seen,
                    write::store_flags(conn, folder, &uids, *add, *remove),
                )
                .await?
            }
            OpAction::Move {
                to_name,
                add,
                remove,
                ..
            } => {
                let messages: Vec<Moving<'_>> = group
                    .messages
                    .iter()
                    .map(|message| Moving {
                        uid: message.uid,
                        message_id: message.message_id.as_deref(),
                    })
                    .collect();
                let request = Move {
                    from: folder,
                    to: to_name,
                    messages: &messages,
                    add: *add,
                    remove: *remove,
                    mark,
                    retry: group.retry || group.attempts > 0,
                };
                race(&self.signals, seen, write::move_messages(conn, request)).await?
            }
            OpAction::Expunge => {
                race(&self.signals, seen, write::expunge(conn, folder, &uids)).await?
            }
            OpAction::Copy { to_name, .. } => {
                let messages: Vec<Moving<'_>> = group
                    .messages
                    .iter()
                    .map(|message| Moving {
                        uid: message.uid,
                        message_id: message.message_id.as_deref(),
                    })
                    .collect();
                let request = Copy {
                    from: folder,
                    to: to_name,
                    messages: &messages,
                    mark,
                    retry: group.retry || group.attempts > 0,
                };
                race(&self.signals, seen, write::copy_messages(conn, request)).await?
            }
            OpAction::ReadBelow { floor } if *floor > 1 => {
                race(
                    &self.signals,
                    seen,
                    write::mark_folder_read(conn, folder, Some(floor - 1)),
                )
                .await?
            }
            OpAction::ReadBelow { .. } => Ok(Done::default()),
        };
        self.used = Instant::now();
        Ok(Outcome::Written(written))
    }

    /// Whether `to` gets its pass now to place what a move or copy put there
    /// ([`place_after_move`]): not Gmail's All Mail while its first sync isn't done, which an
    /// Archive would otherwise do (or go on with) on this connection.
    async fn places(&self, to: FolderId) -> bool {
        let Ok(Some(state)) = self.shared.store.folder_sync(to).recv().await else {
            return true;
        };
        let placed = place_after_move(Facts {
            role: state.role,
            attributes: &[],
            selectable: state.selectable,
            synced_before: state.uidvalidity.is_some(),
            first_synced: state.state == SyncState::Ready,
        });
        if !placed {
            tracing::debug!(
                account = self.account.0,
                %to,
                "a move's target is never synced by a move: no pass"
            );
        }
        placed
    }

    /// Records `done`; replies with the folders the store couldn't place moved messages in.
    async fn record_done(&mut self, group: &OpGroup, done: Done) -> Vec<FolderId> {
        let recorded = self
            .shared
            .store
            .ops_done(OpsDone {
                ops: group.ops.clone(),
                new_uids: done.moved,
                target_uidvalidity: done.target_uidvalidity,
                flagged_only: !done.left_deleted.is_empty(),
            })
            .recv()
            .await;
        let unplaced = match recorded {
            Ok(recorded) => {
                discard(&self.shared, recorded.orphaned_blobs).await;
                recorded.unplaced
            }
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "a done op group couldn't be recorded");
                Vec::new()
            }
        };
        if matches!(group.action, OpAction::Move { .. } | OpAction::Expunge) && self.quota {
            self.quota = false;
            self.status.set(self.healthy());
        }
        unplaced
    }

    /// A refusal or a failure to try again later, by its class.
    async fn group_failed(&mut self, group: &OpGroup, error: &WriteError) -> Ran {
        let attempts = group.attempts + 1;
        let unit = self.jitter.unit();
        let later = |config| {
            let delay = backoff_delay(config, attempts + 1, unit);
            Failure::Transient {
                retry_at: Some(unix_ms() + delay.as_millis() as i64),
            }
        };
        tracing::info!(
            account = self.account.0,
            op = kind(&group.action),
            attempts,
            %error,
            "an op group failed"
        );
        let (failure, ran) = match error.failure() {
            WriteFailure::Retarget(_) => (
                Failure::Transient {
                    retry_at: Some(unix_ms() + self.config().backoff_min.as_millis() as i64),
                },
                Ran::Resync(group.folder),
            ),
            WriteFailure::QuotaFull => {
                self.quota = true;
                self.status.set(SyncStatus::QuotaFull);
                (later(self.config()), Ran::Recorded)
            }
            WriteFailure::Retry if attempts < MAX_ATTEMPTS => (later(self.config()), Ran::Recorded),
            WriteFailure::Retry | WriteFailure::Refuse => (Failure::Permanent, Ran::Recorded),
        };
        self.record_failure(group, failure).await;
        ran
    }

    async fn record_failure(&self, group: &OpGroup, failure: Failure) {
        let ops: Vec<OpId> = group.ops.clone();
        match self.shared.store.ops_failed(ops, failure).recv().await {
            Ok(failed) => self.refusals(&failed.refused),
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "a failed op group couldn't be recorded");
            }
        }
    }
}
