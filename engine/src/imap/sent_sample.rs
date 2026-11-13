//! Metadata-first, memory-only Sent sampling for a writing-style sheet. This module never
//! receives a Store, BlobStore, or filesystem path, and never asks for preview text.

use super::{Connection, ImapError, fetch, raw, sync::view};
use crate::api::Original;
use async_imap::imap_proto::{AttributeValue, BodyStructure, Response};
use sealwax_domain::{message, model::FolderRole};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;

pub const DEFAULT_LIMIT: usize = 300;
pub const MAX_LIMIT: usize = 1_000;
pub const REFRESH_LIMIT: usize = 50;
/// Large attachment messages aren't useful enough to justify downloading their originals.
const MAX_MESSAGE_BYTES: u32 = 256 * 1024;
const HEADER_BATCH: usize = 200;
const BODY_BATCH: usize = 10;
const FIELDS: &str = "DATE FROM TO CC BCC SUBJECT CONTENT-TYPE AUTO-SUBMITTED X-AUTOREPLY X-AUTOREPLY-FROM X-AUTORESPOND";

/// Cancellation covers login, metadata, body fetches and idle waits, by dropping the private
/// connection's future. No connection is reused after interrupting an IMAP command.
#[derive(Clone)]
pub struct Cancel(tokio::sync::watch::Sender<bool>);

impl Default for Cancel {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
}

impl fmt::Debug for Cancel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Cancel").field(&self.is_cancelled()).finish()
    }
}

impl Cancel {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    pub(crate) async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FolderCheckpoint {
    pub name_raw: String,
    pub uidvalidity: u32,
    /// Highest UID examined, including metadata rejected or not chosen for the sample.
    pub last_uid: u32,
    pub last_date: i64,
}

#[derive(Clone, Debug)]
pub struct SampleOptions {
    pub since: i64,
    pub limit: usize,
    pub refresh: bool,
    pub checkpoints: Vec<FolderCheckpoint>,
    pub own_addresses: Vec<String>,
}

impl SampleOptions {
    pub fn cap(&self) -> usize {
        self.limit.min(if self.refresh {
            REFRESH_LIMIT
        } else {
            MAX_LIMIT
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct SentSample {
    /// Raw bytes are transient; `Original`'s Debug prints only its length.
    pub originals: Vec<Original>,
    pub checkpoints: Vec<FolderCheckpoint>,
    pub fetched: usize,
}

/// Body-free metadata used by both remote IMAP and already-local Sent sampling. A local
/// caller can map its message IDs to `uid`; `folder` and `uid` only identify a candidate.
pub struct Candidate {
    pub folder: usize,
    pub uid: u32,
    pub date: i64,
    pub size: u32,
    pub recipients: Vec<String>,
    pub domain: String,
}

/// Header filtering saves body downloads; the app rechecks the complete MIME message and
/// strips quoted material/signatures before deciding whether it has enough authored words.
fn eligible_header(header: &[u8], own: &[String]) -> Option<Vec<String>> {
    let fields = message::header_fields(header);
    let mut is_own = false;
    let mut recipients = Vec::new();
    for field in fields {
        let value = field.value.trim().to_ascii_lowercase();
        match field.name.as_str() {
            "from" => {
                is_own = field.addresses.iter().any(|from| {
                    own.iter()
                        .any(|address| address.trim().eq_ignore_ascii_case(from.trim()))
                })
            }
            "to" | "cc" | "bcc" => {
                recipients.extend(field.addresses.into_iter().map(|r| r.to_ascii_lowercase()))
            }
            "subject" => {
                let mut subject = value.as_str();
                while let Some(rest) = subject.strip_prefix("re:") {
                    subject = rest.trim_start();
                }
                if [
                    "fwd:",
                    "fw:",
                    "accepted:",
                    "declined:",
                    "tentative:",
                    "canceled:",
                    "cancelled:",
                    "out of office",
                    "automatic reply",
                    "auto reply",
                    "autoreply",
                    "ooo:",
                ]
                .iter()
                .any(|prefix| subject.starts_with(prefix))
                    || subject == "ooo"
                {
                    return None;
                }
            }
            "auto-submitted" if !value.is_empty() && value != "no" => return None,
            "x-autoreply" | "x-autoreply-from" | "x-autorespond" if !value.is_empty() => {
                return None;
            }
            "content-type" if value.contains("text/calendar") => return None,
            _ => {}
        }
    }
    recipients.sort();
    recipients.dedup();
    (is_own && !recipients.is_empty()).then_some(recipients)
}

/// A nested calendar part is still a meeting response even when its root is multipart/mixed.
fn calendar(body: &BodyStructure<'_>) -> bool {
    match body {
        BodyStructure::Multipart { bodies, .. } => bodies.iter().any(calendar),
        BodyStructure::Text { common, .. }
        | BodyStructure::Basic { common, .. }
        | BodyStructure::Message { common, .. } => {
            common.ty.ty.eq_ignore_ascii_case("text")
                && common.ty.subtype.eq_ignore_ascii_case("calendar")
        }
    }
}

fn metadata(attributes: &[AttributeValue<'_>], folder: usize, own: &[String]) -> Option<Candidate> {
    if attributes
        .iter()
        .any(|attribute| matches!(attribute, AttributeValue::BodyStructure(body) if calendar(body)))
    {
        return None;
    }
    let fetched = fetch::fetched(attributes)?;
    let size = fetched.size?;
    if !(100..=MAX_MESSAGE_BYTES).contains(&size)
        || !(fetched.structure.plain || fetched.structure.html)
    {
        return None;
    }
    let recipients = eligible_header(&fetched.header, own)?;
    let domain = recipients.first()?.rsplit_once('@')?.1.to_owned();
    Some(Candidate {
        folder,
        uid: fetched.uid,
        date: fetched.internal_date.unwrap_or(0),
        size,
        recipients,
        domain,
    })
}

fn size_bucket(size: u32) -> usize {
    if size < 2_000 {
        0
    } else if size < 8_000 {
        1
    } else {
        2
    }
}

/// Stable shuffling breaks UID/newest bias within each balanced group without a random seed
/// or a process-dependent hash. No mail text, addresses, or account information is hashed.
fn stable_key(folder: usize, uid: u32) -> u64 {
    let mut x = (uid as u64) ^ (folder as u64).wrapping_mul(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

/// Choose a deterministic, diverse sample from body-free metadata, shared with local Sent
/// folders. Callers apply their date/authorship filters first and fetch only these bodies.
/// Refresh callers pass `REFRESH_LIMIT` (or a smaller cap).
pub fn select(mut candidates: Vec<Candidate>, limit: usize) -> Vec<Candidate> {
    let limit = limit.min(MAX_LIMIT);
    if limit == 0 || candidates.is_empty() {
        return Vec::new();
    }
    // An unsolicited repeated FETCH must not turn into a second body request for the UID.
    candidates.sort_by_key(|candidate| (candidate.folder, candidate.uid));
    candidates.dedup_by_key(|candidate| (candidate.folder, candidate.uid));
    candidates.sort_by_key(|candidate| (candidate.date, candidate.folder, candidate.uid));
    let count = candidates.len();
    let mut candidates: Vec<_> = candidates
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| (candidate, index * 4 / count))
        .collect();
    let per_recipient = 15.min(limit.div_ceil(10));
    let mut recipients: HashMap<String, usize> = HashMap::new();
    let mut domains: HashMap<String, usize> = HashMap::new();
    let mut dates = [0usize; 4];
    let mut sizes = [0usize; 3];
    let mut selected = Vec::new();
    while selected.len() < limit {
        let next = candidates
            .iter()
            .enumerate()
            .filter(|(_, (candidate, _))| {
                candidate
                    .recipients
                    .iter()
                    .all(|address| recipients.get(address).copied().unwrap_or(0) < per_recipient)
            })
            .min_by_key(|(_, (candidate, date))| {
                (
                    domains.get(&candidate.domain).copied().unwrap_or(0),
                    dates[*date],
                    sizes[size_bucket(candidate.size)],
                    candidate
                        .recipients
                        .iter()
                        .map(|address| recipients.get(address).copied().unwrap_or(0))
                        .max()
                        .unwrap_or(0),
                    stable_key(candidate.folder, candidate.uid),
                )
            })
            .map(|(index, _)| index);
        let Some(next) = next else {
            break;
        };
        let (candidate, date) = candidates.swap_remove(next);
        for address in &candidate.recipients {
            *recipients.entry(address.clone()).or_default() += 1;
        }
        *domains.entry(candidate.domain.clone()).or_default() += 1;
        dates[date] += 1;
        sizes[size_bucket(candidate.size)] += 1;
        selected.push(candidate);
    }
    selected
}

/// Lists Sent folders, reads only recent metadata, chooses one globally capped sample and
/// downloads exactly those bodies. All state and originals remain in memory.
pub async fn sample(
    conn: &mut Connection,
    options: &SampleOptions,
    mut progress: impl FnMut(usize, usize),
) -> Result<SentSample, ImapError> {
    let mut folders = super::list(conn).await?;
    folders.retain(|folder| folder.selectable && folder.role == FolderRole::Sent);
    folders.sort_by(|a, b| a.name_raw.cmp(&b.name_raw));
    if folders.is_empty() {
        return Err(ImapError::Protocol("no selectable Sent folder"));
    }
    let mut result = SentSample::default();
    let mut candidates = Vec::new();
    for (folder_index, folder) in folders.iter().enumerate() {
        let mut state = view::examine(conn, &folder.name_raw).await?;
        let previous = options
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.name_raw == folder.name_raw);
        let last_uid = previous
            .filter(|checkpoint| checkpoint.uidvalidity == state.uidvalidity)
            .map(|checkpoint| checkpoint.last_uid);
        let last_date = previous.map(|checkpoint| checkpoint.last_date).or_else(|| {
            options
                .refresh
                .then(|| {
                    options
                        .checkpoints
                        .iter()
                        .map(|checkpoint| checkpoint.last_date)
                        .max()
                })
                .flatten()
        });
        let since = options.since.max(last_date.unwrap_or(options.since));
        let mut criteria = format!("SINCE {}", view::imap_date(since));
        if let Some(uid) = last_uid {
            criteria.push_str(&format!(" UID {}:*", uid.saturating_add(1)));
        }
        let mut uids = view::uid_search(conn, &criteria, &mut state).await?;
        uids.retain(|uid| *uid > last_uid.unwrap_or(0));
        let mut checkpoint = FolderCheckpoint {
            name_raw: folder.name_raw.clone(),
            uidvalidity: state.uidvalidity,
            last_uid: uids.last().copied().or(last_uid).unwrap_or(0),
            last_date: last_date.unwrap_or(options.since),
        };
        for batch in uids.chunks(HEADER_BATCH) {
            let wanted: HashSet<u32> = batch.iter().copied().collect();
            let command = format!(
                "UID FETCH {} (UID INTERNALDATE RFC822.SIZE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS ({FIELDS})])",
                raw::uid_set(batch)
            );
            raw::run(&mut conn.session, &command, |response| {
                let Response::Fetch(_, attributes) = response else {
                    return;
                };
                let Some(uid) = attributes.iter().find_map(|attribute| match attribute {
                    AttributeValue::Uid(uid) => Some(*uid),
                    _ => None,
                }) else {
                    return;
                };
                if !wanted.contains(&uid) {
                    return;
                }
                let date = attributes.iter().find_map(|attribute| match attribute {
                    AttributeValue::InternalDate(date) => fetch::internal_date(date),
                    _ => None,
                });
                if let Some(date) = date {
                    checkpoint.last_date = checkpoint.last_date.max(date);
                }
                // SINCE is only day-granular. Enforce the precise lower bound locally and,
                // after UIDVALIDITY changes, don't relearn earlier mail under new UIDs.
                let Some(date) = date.filter(|date| {
                    *date >= options.since
                        && (last_uid.is_some() || last_date.is_none_or(|last| *date > last))
                }) else {
                    return;
                };
                if let Some(mut candidate) =
                    metadata(attributes, folder_index, &options.own_addresses)
                {
                    candidate.date = date;
                    candidates.push(candidate);
                }
            })
            .await?;
        }
        result.checkpoints.push(checkpoint);
    }
    let selected = select(candidates, options.cap());
    progress(0, selected.len());
    for (folder_index, folder) in folders.iter().enumerate() {
        let uids: Vec<_> = selected
            .iter()
            .filter(|candidate| candidate.folder == folder_index)
            .map(|candidate| candidate.uid)
            .collect();
        if uids.is_empty() {
            continue;
        }
        let state = view::examine(conn, &folder.name_raw).await?;
        if state.uidvalidity != result.checkpoints[folder_index].uidvalidity {
            return Err(ImapError::Protocol(
                "Sent UIDVALIDITY changed during sampling",
            ));
        }
        for batch in uids.chunks(BODY_BATCH) {
            let wanted: HashSet<_> = batch.iter().copied().collect();
            let mut fetched = HashSet::new();
            let command = format!("UID FETCH {} (UID BODY.PEEK[])", raw::uid_set(batch));
            raw::run(&mut conn.session, &command, |response| {
                let Response::Fetch(_, attributes) = response else {
                    return;
                };
                let Some(uid) = attributes.iter().find_map(|attribute| match attribute {
                    AttributeValue::Uid(uid) => Some(*uid),
                    _ => None,
                }) else {
                    return;
                };
                if !wanted.contains(&uid) || fetched.contains(&uid) {
                    return;
                }
                if let Some(data) = attributes.iter().find_map(|attribute| match attribute {
                    AttributeValue::BodySection {
                        section: None,
                        data: Some(data),
                        ..
                    } => Some(data),
                    _ => None,
                }) {
                    fetched.insert(uid);
                    result.originals.push(Original(data.to_vec()));
                }
            })
            .await?;
            result.fetched += batch.len();
            progress(result.fetched, selected.len());
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
