//! The account pass: S1's `sync_account` on M2's folder list. LIST with SPECIAL-USE and STATUS
//! in one round trip, pipelined behind an UNSELECT so no STATUS comes from the connection's own
//! selected mailbox (Dovecot answers that from a stale view, S1); the store's folder list made
//! the server's, so folders gone from it leave with their messages; then a folder pass for
//! every folder the policy syncs whose STATUS differs from the store, and for a folder that
//! keeps only its newest mail, the server's counts below its floor from that STATUS (ML-043).
//!
//! The fork's Superhuman pass: the passes go INBOX first, then Sent, then the rest in LIST order
//! (Gmail lists Drafts before Sent); the INBOX is told the moment the list is stored; and on
//! Gmail, whose HIGHESTMODSEQ is the account's (every folder's changes with anything anywhere),
//! the STATUS check compares UNSEEN with the last pass's instead and leaves HIGHESTMODSEQ to the
//! flag sweep.

use super::folder::sync_folder;
use super::{FolderLocks, SyncOptions, SyncReport};
use crate::imap::connection::Connection;
use crate::imap::error::{ImapError, SyncError};
use crate::imap::first_sync::Progress;
use crate::imap::folders::{self, FolderStatus, ListedFolder};
use sealwax_domain::model::{AccountId, FolderId, FolderRole};
use sealwax_domain::sync::{Facts, Plan, plan};
use sealwax_infrastructure::store::{FolderSync, Store, SyncState};
use std::collections::HashMap;
use std::sync::PoisonError;
use std::time::Instant;

/// One account pass. `progress` follows the batches of first syncs. A folder that vanished
/// between LIST and its pass (the server's NO) is skipped; the next pass removes it.
pub async fn sync_account(
    conn: &mut Connection,
    store: &Store,
    account: AccountId,
    options: &SyncOptions,
    locks: &FolderLocks,
    mut progress: impl FnMut(FolderId, Progress),
) -> Result<SyncReport, SyncError> {
    let started = Instant::now();
    let listed = folders::list(conn).await?;
    if listed.is_empty() {
        return Err(ImapError::Protocol("an empty folder list").into());
    }
    let specs = listed
        .iter()
        .map(|folder| folders::spec(account, folder))
        .collect();
    let replaced = store.replace_folders(account, specs).recv().await?;
    let mut report = SyncReport::default();
    report.stats.folders_removed = replaced.removed_folders;
    report.orphaned_blobs = replaced.orphaned_blobs;
    let states: HashMap<FolderId, FolderSync> = store
        .account_folders(account)
        .recv()
        .await?
        .into_iter()
        .map(|state| (state.folder, state))
        .collect();
    let mut order: Vec<(FolderId, &ListedFolder)> =
        replaced.ids.iter().copied().zip(&listed).collect();
    order.sort_by_key(|(_, folder)| first(folder.role));
    if let Some(found) = &options.inbox {
        let inbox = order
            .iter()
            .find(|(_, folder)| folder.role == FolderRole::Inbox && folder.selectable)
            .map(|(id, _)| *id);
        found.send_if_modified(|known| {
            let changed = *known != inbox;
            *known = inbox;
            changed
        });
    }
    // Gmail's HIGHESTMODSEQ is the account's: it can't tell one folder changed.
    let account_modseq = conn.caps().has("X-GM-EXT-1") && !options.flag_sweep;
    for (id, folder) in order {
        let Some(state) = states.get(&id) else {
            continue;
        };
        let mut passed = false;
        if wanted(id, folder, state, options) {
            let skip = folder.status.is_some_and(|status| {
                unchanged(state, &status, account_modseq)
                    && !(account_modseq && unseen_moved(options, id, &status))
            });
            if skip {
                report.stats.status_skips += 1;
            } else {
                match sync_folder(conn, store, id, options, locks, |p| progress(id, p)).await {
                    Ok(done) => {
                        report.add(done);
                        passed = true;
                    }
                    Err(SyncError::Imap(ImapError::No(_))) => report.stats.skipped_missing += 1,
                    Err(error) => return Err(error),
                }
            }
            if skip || passed {
                keep_unseen(options, id, folder.status.as_ref());
            }
        }
        // Whether or not it synced, a windowed folder's counts follow the server's.
        let windowed = state.state == SyncState::Ready && !state.complete();
        if let Some(status) = folder.status.and_then(|status| status.server())
            && (passed || windowed)
            && store.count_below(id, status).recv().await?.value
        {
            report.stats.below_counts += 1;
        }
    }
    tracing::debug!(
        %account,
        folders = listed.len(),
        passes = report.stats.passes,
        status_skips = report.stats.status_skips,
        below_counts = report.stats.below_counts,
        removed_folders = report.stats.folders_removed,
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "account pass done"
    );
    Ok(report)
}

/// The policy syncs the folder here, not on another connection (unless STATUS is the backstop).
fn wanted(id: FolderId, folder: &ListedFolder, state: &FolderSync, options: &SyncOptions) -> bool {
    let facts = Facts {
        role: folder.role,
        attributes: &folder.attributes,
        selectable: folder.selectable,
        synced_before: state.uidvalidity.is_some(),
        first_synced: state.state == SyncState::Ready,
    };
    plan(facts) == Plan::Sync
        && options
            .elsewhere
            .filter(|elsewhere| elsewhere.folder == id)
            .is_none_or(|elsewhere| elsewhere.backstop && folder.status.is_some())
}

/// Where a folder's pass goes in the account pass: INBOX, Sent, then the rest in LIST order.
fn first(role: FolderRole) -> u8 {
    match role {
        FolderRole::Inbox => 0,
        FolderRole::Sent => 1,
        _ => 2,
    }
}

/// The folder's UNSEEN isn't the one the last account pass kept ([`SyncOptions::unseen`]): a
/// message was read or unread elsewhere, which the account's HIGHESTMODSEQ can't tell. Never
/// without a kept map.
fn unseen_moved(options: &SyncOptions, id: FolderId, status: &FolderStatus) -> bool {
    options.unseen.as_ref().is_some_and(|kept| {
        kept.lock().unwrap_or_else(PoisonError::into_inner).get(&id) != status.unseen.as_ref()
    })
}

/// Keeps the folder's UNSEEN once the store holds what its STATUS showed (skipped as
/// unchanged, or passed).
fn keep_unseen(options: &SyncOptions, id: FolderId, status: Option<&FolderStatus>) {
    if let (Some(kept), Some(unseen)) = (&options.unseen, status.and_then(|status| status.unseen)) {
        kept.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, unseen);
    }
}

/// STATUS shows nothing the store doesn't hold. Without HIGHESTMODSEQ a flag change can't
/// show, so such a folder always gets its pass. Where HIGHESTMODSEQ is the account's
/// (`account_modseq`, Gmail outside the flag sweep), it isn't compared: it changes with anything
/// in any folder, which passed every synced folder at every account pass.
fn unchanged(state: &FolderSync, status: &FolderStatus, account_modseq: bool) -> bool {
    state.state == SyncState::Ready
        && status.uidvalidity.is_some()
        && status.uidvalidity == state.uidvalidity
        && status.uidnext == Some(state.uidnext)
        && status.highestmodseq.is_some()
        && (account_modseq || status.highestmodseq == Some(state.highestmodseq))
        && status.messages == Some(state.messages.saturating_add(state.below))
}
