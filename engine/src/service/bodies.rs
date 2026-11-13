//! Body downloads for the app (ML-044): `Command::FetchBodies` answered with one
//! `Event::BodyFetched` per message. A message already queued or in flight isn't asked for twice;
//! each account has one worker with a connection of its own beside the account's sync actor
//! (whose settings it uses and whose reconnects it follows), taking the most wanted messages
//! first, a folder at a time. Its connection stays open `body_linger` after the queue empties,
//! and gets a NOOP before it is used again after sitting idle. The disk cap is enforced when
//! `Command::Storage` arrives and after every batch; after a pass that stored new mail the
//! actor queues the background prefetch behind the reader's requests.
//!
//! The fork's Superhuman pass: what the reader waits for is fetched in batches of its own, never
//! behind or beside background downloads, whose batches stay under [`BACKGROUND_BYTES`]; and a
//! lingering connection is kept warm with a NOOP each `reuse_check`, so a click never waits for
//! a login, nor for that NOOP. It goes at once when the account's actor stops (the account
//! removed, its settings changed), not after the linger.
//!
//! M5 adds the downloads that store nothing (ML-061, ML-063): `Command::FetchPart`, one part of
//! a message stored as its text parts by the section the store recorded, and
//! `Command::FetchOriginal`, a message whole as the server has it. The same worker serves them
//! on the same connection, before any body batch (the reader waits for them; the prefetch
//! doesn't), one at a time, each answered with one `Event::PartFetched` or
//! `Event::OriginalFetched`; asking again while one is out asks nothing more.
//!
//! M8: a POP3 account's mail is in the store whole and its server is never asked: its bodies,
//! parts and originals are answered from the blob store, and a message whose bytes are missing
//! is `Gone`.

use super::actor::Signal;
use super::{Shared, log_in_with_password, log_in_with_token};
use crate::api::{BodyError, Event, FetchedPart, Original, SyncStatus};
use crate::bodies::{enforce_cap, prefetch_ids};
use crate::imap::bodies::{
    BodyFetchError, Sink, fetch_bodies, fetch_examined, fetch_headers, fetch_original, fetch_part,
};
use crate::imap::{Connection, ImapError};
use sealwax_domain::account::{AccountSettings, AuthMethod, LoginFailure, Receive};
use sealwax_domain::bodies::{self as policy, CacheUse, Candidate, Prefetch};
use sealwax_domain::config::Storage;
use sealwax_domain::message;
use sealwax_domain::model::{AccountId, FolderId, MessageId};
use sealwax_infrastructure::store::{BodyState, BodyTarget, FolderSync, PartSource};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, watch};

/// Messages of one folder fetched per batch.
const BATCH: usize = 20;
/// Background downloads per batch, by RFC822.SIZE: what the reader asks for meanwhile waits
/// for at most this much, or for one larger message, which goes alone (whole up to
/// [`WHOLE_LIMIT`](sealwax_domain::bodies::WHOLE_LIMIT), else its text parts).
const BACKGROUND_BYTES: u64 = 2 * 1024 * 1024;
/// New mail fetched on the IDLE connection before it idles again ([`fetch_arrived`]): each
/// message at most this large, and this much in all, so IDLE resumes within a moment.
const ARRIVED_LIMIT: u64 = 1024 * 1024;
const ARRIVED_BYTES: u64 = 2 * 1024 * 1024;
/// The slowest link the inline fetch waits for, in bytes a second (about 1 Mbit/s): its limit
/// grows by the time what it claimed takes at this rate ([`arrived_limit`]).
const ARRIVED_RATE: u64 = 128 * 1024;

#[derive(Default)]
struct State {
    /// Every message asked for and not answered yet, and whether someone waits for its
    /// `BodyFetched` (the background prefetch doesn't).
    asked: HashMap<MessageId, bool>,
    /// Per account, the messages waiting, most wanted first.
    queues: HashMap<AccountId, VecDeque<BodyTarget>>,
    /// Per account, the parts and originals waiting, in the order asked; served before bodies.
    extras: HashMap<AccountId, VecDeque<Extra>>,
    /// Parts (message and mail-parser index) and originals asked for and not answered yet.
    parts: HashSet<(MessageId, u32)>,
    originals: HashSet<MessageId>,
    workers: HashMap<AccountId, tokio::task::AbortHandle>,
    /// Wakes a worker that waits for more work with its connection open.
    wakers: HashMap<AccountId, Arc<Notify>>,
    storage: Option<Storage>,
}

/// A download the reader waits for that stores nothing (M5).
enum Extra {
    /// One part of a message stored as its text parts (ML-061).
    Part { source: PartSource, part: u32 },
    /// A message whole, for Show Original and Save as .eml (ML-063).
    Original { target: BodyTarget },
    /// The headers of messages of one folder, for rules (ADR-020); answered on `reply`, never
    /// as an event.
    Headers {
        folder: FolderId,
        uids: Vec<(MessageId, u32)>,
        reply: tokio::sync::oneshot::Sender<Result<HashMap<MessageId, Vec<u8>>, BodyError>>,
    },
}

/// What an [`Extra`] brought back.
enum Fetched {
    Part(FetchedPart),
    Original(Original),
    Headers(HashMap<MessageId, Vec<u8>>),
}

impl Extra {
    fn folder(&self) -> FolderId {
        match self {
            Extra::Part { source, .. } => source.folder,
            Extra::Original { target } => target.folder,
            Extra::Headers { folder, .. } => *folder,
        }
    }

    /// Its one answer.
    fn answer(self, shared: &Shared, result: Result<Fetched, BodyError>) {
        match self {
            Extra::Part { source, part } => {
                let result = result.and_then(|fetched| match fetched {
                    Fetched::Part(part) => Ok(part),
                    _ => Err(BodyError::Other),
                });
                answer_part(shared, source.message, part, result);
            }
            Extra::Original { target } => {
                let result = result.and_then(|fetched| match fetched {
                    Fetched::Original(original) => Ok(original),
                    _ => Err(BodyError::Other),
                });
                answer_original(shared, target.message, result);
            }
            Extra::Headers { reply, .. } => {
                let result = result.and_then(|fetched| match fetched {
                    Fetched::Headers(headers) => Ok(headers),
                    _ => Err(BodyError::Other),
                });
                let _ = reply.send(result);
            }
        }
    }

    async fn fetch(
        &self,
        conn: &mut Connection,
        folder: &FolderSync,
    ) -> Result<Result<Fetched, BodyError>, ImapError> {
        Ok(match self {
            Extra::Part { source, .. } => {
                fetch_part(conn, folder, source).await?.map(Fetched::Part)
            }
            Extra::Original { target } => fetch_original(conn, folder, target.uid)
                .await?
                .map(|bytes| Fetched::Original(Original(bytes))),
            Extra::Headers { uids, .. } => {
                let wanted: Vec<u32> = uids.iter().map(|(_, uid)| *uid).collect();
                match fetch_headers(conn, folder, &wanted).await? {
                    Some(mut by_uid) => Ok(Fetched::Headers(
                        uids.iter()
                            .filter_map(|(message, uid)| Some((*message, by_uid.remove(uid)?)))
                            .collect(),
                    )),
                    None => Err(BodyError::Gone),
                }
            }
        })
    }
}

#[derive(Default)]
pub(super) struct Bodies {
    state: Mutex<State>,
    /// Blob writes with their upgrades, and evictions with their deletions, one at a time.
    writes: tokio::sync::Mutex<()>,
}

impl Bodies {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stops every worker; their connections close with them.
    pub(super) fn stop(&self) {
        for (_, worker) in self.state().workers.drain() {
            worker.abort();
        }
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

fn sink(shared: &Shared) -> Sink<'_> {
    Sink {
        store: &shared.store,
        blobs: &shared.blobs,
        writes: &shared.bodies.writes,
    }
}

/// Answers `message` once: its `BodyFetched` when someone waits for it.
fn answer(shared: &Shared, message: MessageId, result: Result<(), BodyError>) {
    let announce = shared.bodies.state().asked.remove(&message);
    if announce == Some(true) {
        shared.send(Event::BodyFetched { message, result });
    }
}

/// Queues the bodies of `messages`, the first most wanted, ahead of everything queued before.
/// `announce` is false for the background prefetch, whose messages answer nothing and queue
/// behind everything else; asking for one of them again announces it and moves it to the front.
pub(super) fn request(shared: &Arc<Shared>, messages: Vec<MessageId>, announce: bool) {
    let fresh: Vec<MessageId> = {
        let mut state = shared.bodies.state();
        let mut fresh = Vec::new();
        for message in messages {
            match state.asked.get_mut(&message) {
                Some(announced) => {
                    *announced |= announce;
                    if announce {
                        promote(&mut state, message);
                    }
                }
                None => {
                    state.asked.insert(message, announce);
                    fresh.push(message);
                }
            }
        }
        fresh
    };
    if fresh.is_empty() {
        return;
    }
    let shared = shared.clone();
    tokio::spawn(async move {
        let targets = match shared.store.body_targets(fresh.clone()).recv().await {
            Ok(targets) => targets,
            Err(error) => {
                tracing::warn!(%error, "the messages to download couldn't be read");
                for message in fresh {
                    answer(&shared, message, Err(BodyError::Store));
                }
                return;
            }
        };
        for message in fresh
            .iter()
            .filter(|message| targets.iter().all(|target| target.message != **message))
        {
            answer(&shared, *message, Err(BodyError::Gone));
        }
        let mut queued: HashMap<AccountId, Vec<BodyTarget>> = HashMap::new();
        let mut pop3: HashMap<AccountId, bool> = HashMap::new();
        for target in targets {
            let stored = matches!(target.body_state, BodyState::Full | BodyState::Text)
                && target.blob.is_some_and(|key| shared.blobs.contains(&key));
            if stored {
                answer(&shared, target.message, Ok(()));
                continue;
            }
            let local = match pop3.get(&target.account) {
                Some(local) => *local,
                None => *pop3
                    .entry(target.account)
                    .or_insert(is_pop3(&shared, target.account).await),
            };
            if local {
                answer(&shared, target.message, Err(BodyError::Gone));
            } else {
                queued.entry(target.account).or_default().push(target);
            }
        }
        for (account, targets) in queued {
            enqueue(&shared, account, targets, announce);
        }
    });
}

fn answer_part(
    shared: &Shared,
    message: MessageId,
    part: u32,
    result: Result<FetchedPart, BodyError>,
) {
    shared.bodies.state().parts.remove(&(message, part));
    shared.send(Event::PartFetched {
        message,
        part,
        result,
    });
}

fn answer_original(shared: &Shared, message: MessageId, result: Result<Original, BodyError>) {
    shared.bodies.state().originals.remove(&message);
    shared.send(Event::OriginalFetched { message, result });
}

/// Whether `account` is a POP3 account (M8), whose server is never asked for its mail.
async fn is_pop3(shared: &Shared, account: AccountId) -> bool {
    shared
        .store
        .is_pop3_account(account)
        .recv()
        .await
        .unwrap_or(false)
}

/// A POP3 message's bytes from the blob store; `Gone` without them.
async fn local_raw(shared: &Shared, target: &BodyTarget) -> Result<Vec<u8>, BodyError> {
    let key = target.blob.ok_or(BodyError::Gone)?;
    let blobs = shared.blobs.clone();
    tokio::task::spawn_blocking(move || blobs.get(&key))
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or(BodyError::Gone)
}

/// The part at mail-parser index `part` of a POP3 message, from its bytes in the blob store.
async fn local_part(
    shared: &Shared,
    target: &BodyTarget,
    part: u32,
) -> Result<FetchedPart, BodyError> {
    let raw = local_raw(shared, target).await?;
    tokio::task::spawn_blocking(move || message::part(&raw, part))
        .await
        .ok()
        .flatten()
        .map(|found| FetchedPart {
            mime_type: found.mime_type,
            data: found.data,
        })
        .ok_or(BodyError::Other)
}

/// Where `message` is on the server, if the store still holds it.
async fn stored(shared: &Shared, message: MessageId) -> Result<Option<BodyTarget>, BodyError> {
    match shared.store.body_targets(vec![message]).recv().await {
        Ok(mut targets) => Ok(targets.pop()),
        Err(error) => {
            tracing::warn!(%error, "the message to download couldn't be read");
            Err(BodyError::Store)
        }
    }
}

/// Queues the part at mail-parser index `part` of `message` (M5, ML-061), by the section the
/// store recorded for it; answered with one `PartFetched`. A message the store doesn't hold is
/// `Gone`; a part without a section (its bytes are in the stored message) is `Other`.
pub(super) fn request_part(shared: &Arc<Shared>, message: MessageId, part: u32) {
    if !shared.bodies.state().parts.insert((message, part)) {
        return;
    }
    let shared = shared.clone();
    tokio::spawn(async move {
        let target = match stored(&shared, message).await {
            Ok(Some(target)) => target,
            Ok(None) => return answer_part(&shared, message, part, Err(BodyError::Gone)),
            Err(error) => return answer_part(&shared, message, part, Err(error)),
        };
        if is_pop3(&shared, target.account).await {
            let result = local_part(&shared, &target, part).await;
            return answer_part(&shared, message, part, result);
        }
        let source = match shared.store.part_source(message, part).recv().await {
            Ok(Some(source)) => source,
            Ok(None) => {
                tracing::info!(id = message.0, part, "the part has no section to fetch");
                return answer_part(&shared, message, part, Err(BodyError::Other));
            }
            Err(error) => {
                tracing::warn!(%error, "the part to download couldn't be read");
                return answer_part(&shared, message, part, Err(BodyError::Store));
            }
        };
        enqueue_extra(&shared, target.account, Extra::Part { source, part });
    });
}

/// Queues `message` whole (M5, ML-063); answered with one `OriginalFetched`. A message the
/// store doesn't hold is `Gone`.
pub(super) fn request_original(shared: &Arc<Shared>, message: MessageId) {
    if !shared.bodies.state().originals.insert(message) {
        return;
    }
    let shared = shared.clone();
    tokio::spawn(async move {
        match stored(&shared, message).await {
            Ok(Some(target)) if is_pop3(&shared, target.account).await => {
                let result = local_raw(&shared, &target).await.map(Original);
                answer_original(&shared, message, result);
            }
            Ok(Some(target)) => {
                enqueue_extra(&shared, target.account, Extra::Original { target });
            }
            Ok(None) => answer_original(&shared, message, Err(BodyError::Gone)),
            Err(error) => answer_original(&shared, message, Err(error)),
        }
    });
}

/// The headers of `uids` (messages of `folder` and their UIDs) as the server has them, fetched
/// on the account's body connection before any body batch (rules, ADR-020). Messages the server
/// no longer has are left out.
pub(super) async fn headers(
    shared: &Arc<Shared>,
    account: AccountId,
    folder: FolderId,
    uids: Vec<(MessageId, u32)>,
) -> Result<HashMap<MessageId, Vec<u8>>, BodyError> {
    if uids.is_empty() {
        return Ok(HashMap::new());
    }
    let (reply, answer) = tokio::sync::oneshot::channel();
    enqueue_extra(
        shared,
        account,
        Extra::Headers {
            folder,
            uids,
            reply,
        },
    );
    answer.await.unwrap_or(Err(BodyError::Other))
}

/// Moves a queued message to the front of its account's queue.
fn promote(state: &mut State, message: MessageId) {
    for queue in state.queues.values_mut() {
        if let Some(index) = queue.iter().position(|target| target.message == message) {
            if let Some(target) = queue.remove(index) {
                queue.push_front(target);
            }
            return;
        }
    }
}

/// Adds `targets` in their order: at the front when someone waits for them, else at the back.
fn queue_in(queue: &mut VecDeque<BodyTarget>, targets: Vec<BodyTarget>, front: bool) {
    if front {
        for target in targets.into_iter().rev() {
            queue.push_front(target);
        }
    } else {
        queue.extend(targets);
    }
}

/// [`queue_in`], with a background message that someone asked for while it was on its way to
/// the queue (`asked` says they wait: `request` found nothing queued to move) at the front too,
/// never behind the prefetch.
fn queue_asked(
    queue: &mut VecDeque<BodyTarget>,
    targets: Vec<BodyTarget>,
    front: bool,
    asked: &HashMap<MessageId, bool>,
) {
    let (waited, rest): (Vec<BodyTarget>, Vec<BodyTarget>) = targets
        .into_iter()
        .partition(|target| front || asked.get(&target.message) == Some(&true));
    queue_in(queue, rest, false);
    queue_in(queue, waited, true);
}

fn enqueue(shared: &Arc<Shared>, account: AccountId, targets: Vec<BodyTarget>, front: bool) {
    let mut state = shared.bodies.state();
    {
        let State { queues, asked, .. } = &mut *state;
        queue_asked(queues.entry(account).or_default(), targets, front, asked);
    }
    start(shared, &mut state, account);
}

fn enqueue_extra(shared: &Arc<Shared>, account: AccountId, extra: Extra) {
    let mut state = shared.bodies.state();
    state.extras.entry(account).or_default().push_back(extra);
    start(shared, &mut state, account);
}

/// Wakes the account's worker, or starts one.
fn start(shared: &Arc<Shared>, state: &mut State, account: AccountId) {
    let running = state
        .workers
        .get(&account)
        .is_some_and(|worker| !worker.is_finished());
    if running {
        state.wakers.entry(account).or_default().notify_one();
    } else {
        let task = tokio::spawn(worker(shared.clone(), account));
        state.workers.insert(account, task.abort_handle());
    }
}

/// The most wanted message and up to [`BATCH`] - 1 more of its folder, in queue order: when
/// someone waits for it (`asked`), only messages someone waits for; else background ones, up
/// to [`BACKGROUND_BYTES`] in all.
fn take_batch(
    queue: &mut VecDeque<BodyTarget>,
    asked: &HashMap<MessageId, bool>,
) -> Option<(FolderId, Vec<BodyTarget>)> {
    let waited = |target: &BodyTarget| asked.get(&target.message).copied().unwrap_or(false);
    let first = queue.front()?;
    let (folder, reader) = (first.folder, waited(first));
    let mut batch: Vec<BodyTarget> = Vec::new();
    let mut bytes = 0u64;
    let mut index = 0;
    while index < queue.len() && batch.len() < BATCH {
        let target = &queue[index];
        let fits = reader || batch.is_empty() || bytes + target.size <= BACKGROUND_BYTES;
        if target.folder == folder && waited(target) == reader && fits {
            bytes += target.size;
            batch.extend(queue.remove(index));
        } else {
            index += 1;
        }
    }
    Some((folder, batch))
}

/// What a worker does next: a part or an original first, else a batch of bodies.
enum Work {
    Extra(Extra),
    Bodies(FolderId, Vec<BodyTarget>),
}

/// The next work of `account`, if any.
fn next_work(shared: &Shared, account: AccountId) -> Option<Work> {
    let mut state = shared.bodies.state();
    if let Some(extra) = state.extras.get_mut(&account).and_then(VecDeque::pop_front) {
        return Some(Work::Extra(extra));
    }
    let State { queues, asked, .. } = &mut *state;
    let (folder, batch) = take_batch(queues.get_mut(&account)?, asked)?;
    Some(Work::Bodies(folder, batch))
}

/// Ends the worker if its queue is still empty, under the same lock that `enqueue` takes, so a
/// message queued meanwhile either finds it waiting or starts a new one.
fn retire(shared: &Shared, account: AccountId) -> bool {
    let mut state = shared.bodies.state();
    if state
        .queues
        .get(&account)
        .is_some_and(|queue| !queue.is_empty())
        || state
            .extras
            .get(&account)
            .is_some_and(|extras| !extras.is_empty())
    {
        return false;
    }
    state.queues.remove(&account);
    state.extras.remove(&account);
    state.workers.remove(&account);
    state.wakers.remove(&account);
    true
}

/// The account's waker, for a worker waiting with its connection open.
fn waker(shared: &Shared, account: AccountId) -> Arc<Notify> {
    shared
        .bodies
        .state()
        .wakers
        .entry(account)
        .or_default()
        .clone()
}

/// Resolves once the actor asks for a reconnect after `seen`, or goes away.
async fn reconnected(mut signals: watch::Receiver<Signal>, seen: u64) {
    loop {
        let signal = *signals.borrow_and_update();
        if signal.reconnects != seen || signal.stop.is_some() {
            return;
        }
        if signals.changed().await.is_err() {
            return;
        }
    }
}

/// Resolves once the actor these signals are from stops or goes away; never without one.
async fn stopped(signals: Option<watch::Receiver<Signal>>) {
    let Some(mut signals) = signals else {
        return std::future::pending().await;
    };
    while signals.borrow_and_update().stop.is_none() {
        if signals.changed().await.is_err() {
            return;
        }
    }
}

/// Answers everything `account` has queued with `error`, and ends its worker.
fn fail_all(shared: &Shared, account: AccountId, error: BodyError) {
    let (waiting, extras): (Vec<MessageId>, VecDeque<Extra>) = {
        let mut state = shared.bodies.state();
        state.workers.remove(&account);
        let waiting = state
            .queues
            .remove(&account)
            .map(|queue| queue.into_iter().map(|target| target.message).collect())
            .unwrap_or_default();
        (waiting, state.extras.remove(&account).unwrap_or_default())
    };
    for message in waiting {
        answer(shared, message, Err(error));
    }
    for extra in extras {
        extra.answer(shared, Err(error));
    }
}

/// What a refused or failed login tells the reader.
fn login_error(failure: LoginFailure) -> BodyError {
    match SyncStatus::from_failure(failure) {
        SyncStatus::NeedsSignIn(_) => BodyError::NeedsSignIn,
        SyncStatus::Offline(LoginFailure::Other) => BodyError::Other,
        _ => BodyError::Offline,
    }
}

async fn log_in(shared: &Shared, settings: &AccountSettings) -> Result<Connection, BodyError> {
    let connection = match settings.auth {
        AuthMethod::Password => log_in_with_password(shared, settings).await,
        AuthMethod::OAuth(_) => log_in_with_token(shared, settings).await,
    };
    connection.map_err(|failure| {
        tracing::info!(?failure, "the body download's login failed");
        login_error(failure)
    })
}

/// One account's downloads, until its queue has been empty for `body_linger`.
async fn worker(shared: Arc<Shared>, account: AccountId) {
    let Some(settings) = shared.actor_settings(account) else {
        tracing::info!(
            account = account.0,
            "bodies asked for an account with no server"
        );
        fail_all(&shared, account, BodyError::Other);
        return;
    };
    if let Receive::Pop3(_) = settings.receive {
        // A POP3 account's mail is in the store whole: its server has nothing to fetch by part.
        tracing::info!(account = account.0, "bodies asked for a POP3 account");
        fail_all(&shared, account, BodyError::Gone);
        return;
    }
    let signals = shared.actor_signal(account);
    let mut seen = signals
        .as_ref()
        .map_or(0, |signals| signals.borrow().reconnects);
    let mut connection: Option<Connection> = None;
    let mut used = Instant::now();
    // When the worker ends if no work comes; set while it waits with nothing to do.
    let mut lingering: Option<Instant> = None;
    loop {
        let Some(work) = next_work(&shared, account) else {
            let ends = *lingering.get_or_insert_with(|| Instant::now() + shared.sync.body_linger);
            // Kept warm: a NOOP whenever the connection would need one before its next use.
            let warm = connection
                .as_ref()
                .map(|_| used + shared.sync.reuse_check)
                .filter(|due| *due < ends);
            let wake = warm.unwrap_or(ends);
            let notify = waker(&shared, account);
            let actor_stopped = tokio::select! {
                () = notify.notified() => continue,
                () = tokio::time::sleep_until(wake.into()) => false,
                // The account removed, its settings changed: the connection goes now, not after
                // the linger; more work starts a new worker, with the new actor's settings.
                () = stopped(signals.clone()) => true,
            };
            if actor_stopped {
                if retire(&shared, account) {
                    break;
                }
                continue;
            }
            if warm.is_none() {
                if retire(&shared, account) {
                    break;
                }
                lingering = None;
                continue;
            }
            let reconnected = signals
                .as_ref()
                .is_some_and(|signals| signals.borrow().reconnects != seen);
            if let Some(conn) = connection.as_mut().filter(|_| !reconnected) {
                let alive = tokio::time::timeout(shared.sync.noop_timeout, conn.noop()).await;
                used = Instant::now();
                if !matches!(alive, Ok(Ok(_))) {
                    connection = None;
                }
            } else {
                connection = None;
            }
            continue;
        };
        lingering = None;
        if let Some(signals) = &signals {
            let now = signals.borrow().reconnects;
            if now != seen {
                connection = None;
                seen = now;
            }
        }
        if let Some(conn) = connection.as_mut()
            && used.elapsed() >= shared.sync.reuse_check
        {
            let alive = tokio::time::timeout(shared.sync.noop_timeout, conn.noop()).await;
            if !matches!(alive, Ok(Ok(_))) {
                connection = None;
            }
        }
        let (folder, batch) = match work {
            Work::Bodies(folder, batch) => (folder, batch),
            Work::Extra(extra) => {
                let ran = run_extra(
                    &shared,
                    &settings,
                    signals.as_ref(),
                    &mut seen,
                    &mut connection,
                    extra,
                )
                .await;
                used = Instant::now();
                if let Err(error) = ran {
                    fail_all(&shared, account, error);
                    return;
                }
                continue;
            }
        };
        let Some(sync) = shared.store.folder_sync(folder).recv().await.ok().flatten() else {
            for target in &batch {
                answer(&shared, target.message, Err(BodyError::Gone));
            }
            continue;
        };
        let fetched_now: Vec<MessageId> = batch.iter().map(|target| target.message).collect();
        let mut left = batch;
        let mut attempts = 0;
        while !left.is_empty() {
            attempts += 1;
            let conn = match connection.as_mut() {
                Some(conn) => conn,
                None => match log_in(&shared, &settings).await {
                    Ok(conn) => connection.insert(conn),
                    Err(error) => {
                        for target in &left {
                            answer(&shared, target.message, Err(error));
                        }
                        fail_all(&shared, account, error);
                        return;
                    }
                },
            };
            let mut answered = Vec::new();
            let fetch = fetch_bodies(conn, sink(&shared), &sync, &left, now(), |id, result| {
                answered.push(id);
                answer(&shared, id, result);
            });
            // A reconnect (resume, network back) abandons a fetch on a connection that may be
            // dead; the batch is tried once more on a new one.
            let fetched = match &signals {
                Some(signals) => tokio::select! {
                    fetched = fetch => fetched,
                    () = reconnected(signals.clone(), seen) => {
                        seen = signals.borrow().reconnects;
                        Err(BodyFetchError::Imap(ImapError::ConnectionLost))
                    }
                },
                None => fetch.await,
            };
            used = Instant::now();
            left.retain(|target| !answered.contains(&target.message));
            let Err(error) = fetched else {
                continue;
            };
            let connection_lost =
                matches!(&error, BodyFetchError::Imap(failure) if failure.is_connection());
            if connection_lost {
                connection = None;
            }
            if !connection_lost || attempts >= 2 {
                tracing::info!(%error, "bodies couldn't be downloaded");
                for target in left.drain(..) {
                    answer(&shared, target.message, Err(error.body_error()));
                }
            }
        }
        cap(&shared, &fetched_now).await;
    }
    if let Some(connection) = connection {
        connection.logout().await;
    }
}

/// One part or original on the worker's connection, tried once more on a new connection after
/// a cut or a reconnect. `Err` when the login failed: the account's other work fails with it.
async fn run_extra(
    shared: &Shared,
    settings: &AccountSettings,
    signals: Option<&watch::Receiver<Signal>>,
    seen: &mut u64,
    connection: &mut Option<Connection>,
    extra: Extra,
) -> Result<(), BodyError> {
    let Some(folder) = shared
        .store
        .folder_sync(extra.folder())
        .recv()
        .await
        .ok()
        .flatten()
    else {
        extra.answer(shared, Err(BodyError::Gone));
        return Ok(());
    };
    for attempt in 1..=2 {
        let conn = match connection.as_mut() {
            Some(conn) => conn,
            None => match log_in(shared, settings).await {
                Ok(conn) => connection.insert(conn),
                Err(error) => {
                    extra.answer(shared, Err(error));
                    return Err(error);
                }
            },
        };
        let fetch = extra.fetch(conn, &folder);
        let fetched = match signals {
            Some(signals) => tokio::select! {
                fetched = fetch => fetched,
                () = reconnected(signals.clone(), *seen) => {
                    *seen = signals.borrow().reconnects;
                    Err(ImapError::ConnectionLost)
                }
            },
            None => fetch.await,
        };
        match fetched {
            Ok(result) => {
                extra.answer(shared, result);
                return Ok(());
            }
            Err(error) => {
                let lost = error.is_connection();
                if lost {
                    *connection = None;
                }
                if !lost || attempt == 2 {
                    tracing::info!(%error, "a part or an original couldn't be downloaded");
                    let answer = if lost {
                        BodyError::Offline
                    } else {
                        BodyError::Other
                    };
                    extra.answer(shared, Err(answer));
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// Enforces the disk cap once the settings are known, sparing `keep`.
async fn cap(shared: &Shared, keep: &[MessageId]) {
    let limit = shared
        .bodies
        .state()
        .storage
        .and_then(|storage| storage.cache_size);
    if let Some(limit) = limit
        && let Err(error) = enforce_cap(sink(shared), limit, keep).await
    {
        tracing::warn!(%error, "the disk cap couldn't be enforced");
    }
}

/// New mail's bodies on the connection whose pass just stored it, with its folder still open
/// (the IDLE connection, INBOX), before it idles again: a new message opens at once rather than
/// after the body connection's login (the fork's Superhuman pass). Within the prefetch's rules
/// (on, within half the cap), messages up to [`ARRIVED_LIMIT`], [`ARRIVED_BYTES`] in all; what
/// the body worker has queued or in flight stays its own, and a reader asking meanwhile is
/// answered. What is left when the fetch fails (the error comes back), takes longer than
/// `limit` (`SyncConfig::inline_bodies`; a timeout, which leaves the connection in no known
/// state) or is dropped halfway goes to the body worker ([`Claim`]). Only the FETCH has the
/// limit: the store's reads and the cache's cap around it aren't the connection's, and a slow
/// disk mustn't drop a healthy IDLE connection. Notices its FETCH reads mark the connection
/// [`noticed`](Connection::noticed), for the caller to pass again.
pub(super) async fn fetch_arrived(
    shared: &Arc<Shared>,
    conn: &mut Connection,
    folder: FolderId,
    arrived: Vec<MessageId>,
    limit: Duration,
) -> Result<(), BodyFetchError> {
    let Some(storage) = shared.bodies.state().storage else {
        return Ok(());
    };
    let rules = Prefetch::new(&storage);
    if arrived.is_empty() || !rules.on() {
        return Ok(());
    }
    let (Ok(targets), Ok(usage), Ok(Some(sync))) = (
        shared.store.body_targets(arrived).recv().await,
        shared.store.body_usage().recv().await,
        shared.store.folder_sync(folder).recv().await,
    ) else {
        return Ok(());
    };
    let candidates: Vec<Candidate> = targets
        .iter()
        .filter(|target| target.folder == folder && target.blob.is_none())
        .map(|target| Candidate {
            message: target.message,
            role: sync.role,
            date: 0,
            size: target.size,
            downloaded: false,
        })
        .collect();
    let cache = CacheUse {
        used: usage.bytes,
        limit: storage.cache_size,
    };
    let wanted = policy::prefetch(&rules, cache, &candidates);
    let mut bytes = 0;
    // Claimed in `asked`, as the worker's are, and in the guard that hands back what is left
    // in the same breath: no await between them, so no cut can leave a claim behind.
    let (claimed, mut claim): (Vec<BodyTarget>, Claim<'_>) = {
        let mut state = shared.bodies.state();
        let claimed: Vec<BodyTarget> = targets
            .into_iter()
            .filter(|target| {
                let fits = wanted.contains(&target.message)
                    && target.size <= ARRIVED_LIMIT
                    && bytes + target.size <= ARRIVED_BYTES
                    && !state.asked.contains_key(&target.message);
                if fits {
                    bytes += target.size;
                    state.asked.insert(target.message, false);
                }
                fits
            })
            .collect();
        let left = claimed.iter().map(|target| target.message).collect();
        (claimed, Claim { shared, left })
    };
    if claimed.is_empty() {
        return Ok(());
    }
    let limit = arrived_limit(limit, bytes);
    let fetch = fetch_examined(conn, sink(shared), &sync, &claimed, now(), |id, result| {
        claim.left.retain(|left| *left != id);
        answer(shared, id, result);
    });
    let fetched = tokio::time::timeout(limit, fetch)
        .await
        .unwrap_or(Err(BodyFetchError::Imap(ImapError::Timeout)));
    let answered: Vec<MessageId> = claimed
        .iter()
        .map(|target| target.message)
        .filter(|id| !claim.left.contains(id))
        .collect();
    drop(claim);
    tracing::debug!(
        %folder,
        fetched = answered.len(),
        "new mail's bodies fetched where it arrived"
    );
    cap(shared, &answered).await;
    fetched
}

/// How long the inline fetch of `bytes` may take: `base` (`SyncConfig::inline_bodies`, the
/// round trips) and the time `bytes` take at [`ARRIVED_RATE`]. A fixed 8 s for up to 2 MiB cut
/// a large arrival off on links under about 2 Mbit/s: the IDLE connection connected again and
/// the body connection downloaded the message once more (phase B's review). A stall still ends
/// within it: 24 s for the 2 MiB at most it claims, 8.5 s for a typical 64 KiB.
fn arrived_limit(base: Duration, bytes: u64) -> Duration {
    base + Duration::from_millis(bytes.saturating_mul(1000) / ARRIVED_RATE)
}

/// What [`fetch_arrived`] claimed and hasn't answered. Whatever is left when it ends goes to
/// the body worker, also when the IDLE connection drops the fetch halfway (a reconnect after a
/// resume, the actor stopping): a claim left in `asked` would make every later request for
/// that message wait for an answer that never comes.
struct Claim<'a> {
    shared: &'a Arc<Shared>,
    left: Vec<MessageId>,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let left = std::mem::take(&mut self.left);
        if !left.is_empty() {
            hand_back(self.shared, left);
        }
    }
}

/// Messages [`fetch_arrived`] claimed and couldn't answer go to the body worker, as asked.
fn hand_back(shared: &Arc<Shared>, messages: Vec<MessageId>) {
    let (mut waited, mut background) = (Vec::new(), Vec::new());
    {
        let mut state = shared.bodies.state();
        for id in messages {
            match state.asked.remove(&id) {
                Some(true) => waited.push(id),
                Some(false) => background.push(id),
                None => {}
            }
        }
    }
    for (list, announce) in [(waited, true), (background, false)] {
        if !list.is_empty() {
            request(shared, list, announce);
        }
    }
}

/// Queues the background prefetch (the Inbox and the rest of its conversations) behind the
/// reader's requests, once the `[storage]` settings are known.
pub(super) fn prefetch(shared: &Arc<Shared>) {
    let Some(storage) = shared.bodies.state().storage else {
        return;
    };
    let shared = shared.clone();
    tokio::spawn(async move {
        match prefetch_ids(&shared.store, &storage).await {
            Ok(ids) if !ids.is_empty() => request(&shared, ids, false),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "the prefetch couldn't be planned"),
        }
    });
}

/// New `[storage]` settings: the cap is enforced at once, and the prefetch planned again where
/// accounts sync already (settings read after their first pass; at the start they come first,
/// and each account's first pass plans it).
pub(super) fn set_storage(shared: &Arc<Shared>, storage: Storage) {
    let changed = shared.bodies.state().storage.replace(storage) != Some(storage);
    if changed {
        let capped = shared.clone();
        tokio::spawn(async move { cap(&capped, &[]).await });
        if !shared.actors().is_empty() {
            prefetch(shared);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inline_fetch_waits_as_long_as_its_bytes_take_on_a_slow_link() {
        let base = Duration::from_secs(8);
        assert_eq!(arrived_limit(base, 0), base);
        assert_eq!(arrived_limit(base, 64 * 1024), Duration::from_millis(8_500));
        // The most it claims: 2 MiB at about 1 Mbit/s, where 8 s alone cut it off.
        assert_eq!(arrived_limit(base, ARRIVED_BYTES), Duration::from_secs(24));
        assert!(arrived_limit(base, ARRIVED_BYTES) > Duration::from_secs(16));
        assert_eq!(
            arrived_limit(Duration::from_millis(500), u64::MAX),
            Duration::from_millis(500) + Duration::from_millis(u64::MAX / ARRIVED_RATE)
        );
    }

    fn target(message: i64, folder: i64) -> BodyTarget {
        BodyTarget {
            message: MessageId(message),
            account: AccountId(1),
            folder: FolderId(folder),
            uid: message as u32,
            size: 100,
            body_state: BodyState::Partial,
            blob: None,
        }
    }

    fn messages(queue: &VecDeque<BodyTarget>) -> Vec<i64> {
        queue.iter().map(|target| target.message.0).collect()
    }

    #[test]
    fn the_reader_goes_first_and_the_prefetch_last() {
        let mut queue = VecDeque::new();
        queue_in(&mut queue, vec![target(1, 1), target(2, 1)], false);
        queue_in(&mut queue, vec![target(3, 1), target(4, 1)], true);
        queue_in(&mut queue, vec![target(5, 1)], false);
        queue_in(&mut queue, vec![target(6, 1)], true);
        assert_eq!(messages(&queue), [6, 3, 4, 1, 2, 5]);
        let mut state = State::default();
        state.queues.insert(AccountId(1), queue);
        promote(&mut state, MessageId(5));
        assert_eq!(messages(&state.queues[&AccountId(1)]), [5, 6, 3, 4, 1, 2]);
    }

    #[test]
    fn a_batch_is_the_first_message_and_more_of_its_folder() {
        let mut queue: VecDeque<BodyTarget> = [(1, 7), (2, 8), (3, 7)]
            .into_iter()
            .map(|(message, folder)| target(message, folder))
            .chain((10..40).map(|message| target(message, 7)))
            .collect();
        let take_batch = |queue: &mut VecDeque<BodyTarget>| take_batch(queue, &HashMap::new());
        let (folder, batch) = take_batch(&mut queue).unwrap();
        assert_eq!(folder, FolderId(7));
        assert_eq!(batch.len(), BATCH);
        assert_eq!(
            batch
                .iter()
                .take(3)
                .map(|t| t.message.0)
                .collect::<Vec<_>>(),
            [1, 3, 10]
        );
        assert_eq!(
            queue.front().map(|t| t.message.0),
            Some(2),
            "the other folder waits"
        );
        let (folder, batch) = take_batch(&mut queue).unwrap();
        assert_eq!((folder, batch.len()), (FolderId(8), 1));
        assert_eq!(take_batch(&mut queue).unwrap().1.len(), 30 + 2 - BATCH);
        assert!(take_batch(&mut queue).is_none());
    }

    /// The fork's Superhuman pass: what the reader waits for goes in batches of its own, and
    /// background batches stay under [`BACKGROUND_BYTES`].
    #[test]
    fn the_readers_messages_go_alone_and_background_batches_stay_small() {
        let sized = |message, size| BodyTarget {
            size,
            ..target(message, 7)
        };
        let mut queue: VecDeque<BodyTarget> = [
            sized(1, 100),
            sized(2, 900_000),
            sized(3, 100),
            sized(4, 900_000),
            sized(5, 900_000),
            sized(6, 3_000_000),
            sized(7, 100),
        ]
        .into_iter()
        .collect();
        let asked: HashMap<MessageId, bool> = [(1, true), (3, true)]
            .into_iter()
            .chain((2..=7).filter(|n| *n != 3).map(|n| (n, false)))
            .map(|(n, waits)| (MessageId(n), waits))
            .collect();
        let ids = |batch: Vec<BodyTarget>| batch.iter().map(|t| t.message.0).collect::<Vec<_>>();
        let (_, batch) = take_batch(&mut queue, &asked).unwrap();
        assert_eq!(ids(batch), [1, 3], "the reader's, without the prefetch's");
        let (_, batch) = take_batch(&mut queue, &asked).unwrap();
        assert_eq!(ids(batch), [2, 4, 7], "2 MiB at most");
        let (_, batch) = take_batch(&mut queue, &asked).unwrap();
        assert_eq!(ids(batch), [5]);
        let (_, batch) = take_batch(&mut queue, &asked).unwrap();
        assert_eq!(ids(batch), [6], "a larger one alone");
        assert!(take_batch(&mut queue, &asked).is_none());
    }

    /// A background message the reader asked for while it was on its way to the queue (a
    /// prefetch, or new mail the IDLE connection handed back) goes to the front all the same.
    #[test]
    fn a_message_asked_for_on_its_way_goes_to_the_front() {
        let mut queue = VecDeque::new();
        queue_in(&mut queue, vec![target(1, 1), target(2, 1)], false);
        let asked: HashMap<MessageId, bool> = [(3, false), (4, true), (5, false)]
            .into_iter()
            .map(|(n, waits)| (MessageId(n), waits))
            .collect();
        let arriving = vec![target(3, 1), target(4, 1), target(5, 1)];
        queue_asked(&mut queue, arriving, false, &asked);
        assert_eq!(messages(&queue), [4, 1, 2, 3, 5]);
        queue_asked(&mut queue, vec![target(6, 1), target(7, 1)], true, &asked);
        assert_eq!(
            messages(&queue),
            [6, 7, 4, 1, 2, 3, 5],
            "asked for: all in front"
        );
    }
}
