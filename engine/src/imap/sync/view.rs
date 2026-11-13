//! What the server says about the examined mailbox, kept current from every untagged EXISTS,
//! EXPUNGE and VANISHED a pass reads, and the commands a pass reads it with: EXAMINE, UID
//! SEARCH and the two kinds of UID FETCH. Server notices only ever feed this view; what a pass
//! concludes comes from the state it fetches (ADR-006).

use crate::imap::connection::Connection;
use crate::imap::error::{ImapError, SyncError};
use crate::imap::fetch::{self, Fetched};
use crate::imap::raw::{self, quote};
use async_imap::imap_proto::{AttributeValue, MailboxDatum, Response, ResponseCode, Status};
use sealwax_domain::model::{Flags, FolderId, FolderRole};
use sealwax_domain::split::Split;
use sealwax_infrastructure::store::{FlagUpdate, FolderSync, NewMessage};
use std::collections::{BTreeMap, HashSet};
use std::ops::RangeInclusive;

#[derive(Clone, Debug, Default)]
pub(crate) struct View {
    pub uidvalidity: u32,
    pub uidnext: Option<u32>,
    pub exists: u32,
    pub highestmodseq: Option<u64>,
    /// UIDs reported gone (VANISHED, with or without EARLIER) and not yet applied, as ranges:
    /// a server may name UIDs that never existed, so they are only ever matched against stored
    /// ones.
    pub vanished: Vec<RangeInclusive<u32>>,
    /// The mailbox is Gmail's INBOX: the rows fetched are given Gmail's category
    /// ([`primary`]), until the server refuses the search.
    pub categories: bool,
}

impl View {
    pub(crate) fn track(&mut self, response: &Response<'_>) {
        match response {
            Response::MailboxData(MailboxDatum::Exists(count)) => self.exists = *count,
            Response::Expunge(_) => self.exists = self.exists.saturating_sub(1),
            Response::Vanished { earlier, uids } => {
                if !earlier {
                    let gone: u64 = uids
                        .iter()
                        .map(|range| u64::from(range.end().saturating_sub(*range.start())) + 1)
                        .sum();
                    self.exists = self
                        .exists
                        .saturating_sub(u32::try_from(gone).unwrap_or(u32::MAX));
                }
                self.vanished.extend(uids.iter().cloned());
            }
            _ => {}
        }
    }

    /// Whether `uid` was reported gone.
    pub(crate) fn has_vanished(&self, uid: u32) -> bool {
        self.vanished.iter().any(|range| range.contains(&uid))
    }
}

/// EXAMINE, with CONDSTORE's parameter where the server offers it and QRESYNC isn't enabled
/// (which turns CONDSTORE on by itself).
pub(crate) async fn examine(conn: &mut Connection, name_raw: &str) -> Result<View, ImapError> {
    let mut command = format!("EXAMINE {}", quote(name_raw)?);
    if conn.caps().has("CONDSTORE") && !conn.qresync() {
        command.push_str(" (CONDSTORE)");
    }
    conn.selected = None;
    conn.noticed = false;
    let mut view = View::default();
    let mut uidvalidity = None;
    raw::run(&mut conn.session, &command, |response| match response {
        Response::Data {
            status: Status::Ok,
            outcome,
        } => match outcome.code {
            Some(ResponseCode::UidValidity(value)) => uidvalidity = Some(value),
            Some(ResponseCode::UidNext(value)) => view.uidnext = Some(value),
            Some(ResponseCode::HighestModSeq(value)) => view.highestmodseq = Some(value),
            _ => {}
        },
        other => view.track(other),
    })
    .await?;
    conn.selected = Some(name_raw.to_owned());
    view.uidvalidity = uidvalidity.ok_or(ImapError::Protocol("EXAMINE without UIDVALIDITY"))?;
    view.vanished.clear();
    Ok(view)
}

/// Whether a pass over `state`'s folder gives the rows it fetches Gmail's categories (the fork's
/// Split Inbox, docs/superhuman/split.md): Gmail's INBOX (X-GM-EXT-1), unless the server
/// refused the search on this connection.
pub(crate) fn categorizes(conn: &Connection, state: &FolderSync) -> bool {
    state.role == FolderRole::Inbox && conn.caps().has("X-GM-EXT-1") && !conn.categories.refused
}

/// The UIDs among `uids` in Gmail's Primary tab, by one `UID SEARCH UID <set> X-GM-RAW
/// "category:primary"` (a plain SEARCH response; ESEARCH is never asked for). `None` when the
/// view doesn't ask, or when the server refuses the search: the view and the connection stop
/// asking, and the rows keep to the header rules.
pub(crate) async fn primary(
    conn: &mut Connection,
    uids: &[u32],
    view: &mut View,
) -> Result<Option<HashSet<u32>>, ImapError> {
    if !view.categories || uids.is_empty() {
        return Ok(None);
    }
    let criteria = format!("UID {} X-GM-RAW \"category:primary\"", raw::uid_set(uids));
    match uid_search(conn, &criteria, view).await {
        Ok(found) => Ok(Some(found.into_iter().collect())),
        Err(error) if !error.is_connection() => {
            tracing::info!(%error, "no categories from Gmail; the header rules sort its mail");
            view.categories = false;
            conn.categories.refused = true;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Gives each row its category from `primary`, the UIDs among them in Gmail's Primary tab:
/// Important there, Other in any other tab.
pub(crate) fn categorize(rows: &mut [NewMessage], primary: Option<&HashSet<u32>>) {
    let Some(primary) = primary else {
        return;
    };
    for row in rows {
        if let Some(uid) = row.uid {
            row.category = Some(Split::of(primary.contains(&uid)));
        }
    }
}

/// `UID SEARCH <criteria>`: the matching UIDs, ascending.
pub(crate) async fn uid_search(
    conn: &mut Connection,
    criteria: &str,
    view: &mut View,
) -> Result<Vec<u32>, ImapError> {
    let mut uids = Vec::new();
    raw::run(
        &mut conn.session,
        &format!("UID SEARCH {criteria}"),
        |response| match response {
            Response::MailboxData(MailboxDatum::Search(found)) => uids.extend_from_slice(found),
            other => view.track(other),
        },
    )
    .await?;
    uids.sort_unstable();
    uids.dedup();
    Ok(uids)
}

/// A FETCH response's UID and flags, when it carries both.
fn flag_update(attributes: &[AttributeValue<'_>]) -> Option<FlagUpdate> {
    let mut uid = None;
    let mut flags = None;
    for attribute in attributes {
        match attribute {
            AttributeValue::Uid(found) => uid = Some(*found),
            AttributeValue::Flags(found) => {
                flags = Some(Flags::from_imap(found.iter().map(|flag| flag.as_ref())));
            }
            _ => {}
        }
    }
    let (flags, keywords) = flags?;
    Some(FlagUpdate {
        uid: uid?,
        flags,
        keywords,
    })
}

/// A `UID FETCH … (UID FLAGS)` with its modifiers: the flags of every UID in `range`, the last
/// response for a UID winning. Responses outside `range` are hints and are dropped.
pub(crate) async fn fetch_flags(
    conn: &mut Connection,
    command: &str,
    range: RangeInclusive<u32>,
    view: &mut View,
) -> Result<Vec<FlagUpdate>, ImapError> {
    let mut found = BTreeMap::new();
    raw::run(&mut conn.session, command, |response| match response {
        Response::Fetch(_, attributes) => {
            if let Some(update) = flag_update(attributes).filter(|u| range.contains(&u.uid)) {
                found.insert(update.uid, update);
            }
        }
        other => view.track(other),
    })
    .await?;
    Ok(found.into_values().collect())
}

/// The first sync's header FETCH of `set`, as store rows. Responses for UIDs `wanted` refuses,
/// or without the header section (unsolicited flag updates), are hints and are dropped. In
/// Gmail's INBOX the rows come with their categories ([`primary`]), so new mail is stored on
/// its side of the Split Inbox at once.
pub(crate) async fn fetch_rows(
    conn: &mut Connection,
    folder: FolderId,
    set: &str,
    wanted: impl Fn(u32) -> bool,
    view: &mut View,
) -> Result<Vec<NewMessage>, SyncError> {
    let mut out: Vec<Fetched> = Vec::new();
    let mut seen = HashSet::new();
    let command = format!("UID FETCH {set} {}", fetch::items());
    raw::run(&mut conn.session, &command, |response| match response {
        Response::Fetch(_, attributes) => {
            if let Some(found) = fetch::fetched(attributes)
                && wanted(found.uid)
                && seen.insert(found.uid)
            {
                out.push(found);
            }
        }
        other => view.track(other),
    })
    .await?;
    let uids: Vec<u32> = out.iter().map(|found| found.uid).collect();
    // The rows are parsed while Gmail is asked for their categories.
    let parsed = tokio::task::spawn_blocking(move || {
        out.into_iter()
            .map(|found| fetch::message(folder, found))
            .collect::<Vec<_>>()
    });
    let primary = primary(conn, &uids, view).await?;
    let mut rows = parsed.await.map_err(|_| SyncError::Parse)?;
    categorize(&mut rows, primary.as_ref());
    Ok(rows)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A SEARCH date (`7-Jul-2026`) for the day `unix` (seconds, UTC) falls on; the epoch for
/// anything earlier.
pub(crate) fn imap_date(unix: i64) -> String {
    let days = unix.max(0).div_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{day}-{}-{year}", MONTHS[(month - 1) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imap::{Credentials, connect};
    use crate::net::testing::{Peer, bind, test_ca};
    use sealwax_domain::account::{Endpoint, Security};
    use sealwax_infrastructure::store::SyncState;

    /// A scripted Gmail's half of a login, with Gmail's capabilities after it.
    async fn gmail(
        listener: &tokio::net::TcpListener,
        config: std::sync::Arc<rustls::ServerConfig>,
    ) -> Peer {
        let mut peer = Peer::accept_tls(listener, config).await;
        peer.send("* OK Gimap ready for requests from 127.0.0.1\r\n")
            .await;
        let tag = peer.expect("CAPABILITY").await;
        peer.send("* CAPABILITY IMAP4rev1 UNSELECT IDLE NAMESPACE QUOTA ID XLIST CHILDREN X-GM-EXT-1 XYZZY SASL-IR AUTH=PLAIN\r\n")
            .await;
        peer.send(&format!("{tag} OK Thats all she wrote!\r\n"))
            .await;
        let tag = peer.expect("AUTHENTICATE PLAIN").await;
        peer.send(&format!(
            "{tag} OK [CAPABILITY IMAP4rev1 UNSELECT IDLE NAMESPACE QUOTA ID XLIST CHILDREN \
             X-GM-EXT-1 UIDPLUS COMPRESS=DEFLATE ENABLE MOVE CONDSTORE ESEARCH UTF8=ACCEPT \
             LIST-EXTENDED LIST-STATUS LITERAL- SPECIAL-USE APPENDLIMIT=35651584] \
             user@example.test authenticated (Success)\r\n"
        ))
        .await;
        while let Some(line) = peer.line().await {
            let (tag, command) = line.trim_end().split_once(' ').unwrap_or_default();
            if command.eq_ignore_ascii_case("CAPABILITY") {
                peer.send(
                    "* CAPABILITY IMAP4rev1 UNSELECT IDLE NAMESPACE QUOTA ID XLIST CHILDREN \
                           X-GM-EXT-1 UIDPLUS COMPRESS=DEFLATE ENABLE MOVE CONDSTORE ESEARCH \
                           UTF8=ACCEPT LIST-EXTENDED LIST-STATUS LITERAL- SPECIAL-USE\r\n",
                )
                .await;
                peer.send(&format!("{tag} OK Success\r\n")).await;
            } else if command.to_ascii_uppercase().starts_with("ID ") {
                peer.send("* ID (\"name\" \"GImap\" \"vendor\" \"Google, Inc.\")\r\n")
                    .await;
                peer.send(&format!("{tag} OK Success\r\n")).await;
                break;
            } else {
                peer.send(&format!("{tag} OK Success\r\n")).await;
                break;
            }
        }
        peer
    }

    async fn client(port: u16, trust: &crate::net::Trust) -> Connection {
        connect(
            &Endpoint {
                host: "127.0.0.1".into(),
                port,
                security: Security::Tls,
                certificate: None,
            },
            Credentials::Password {
                username: "user@example.test",
                password: "secret-password",
            },
            trust.client_config().unwrap(),
        )
        .await
        .unwrap()
    }

    fn inbox() -> FolderSync {
        FolderSync {
            folder: FolderId(1),
            account: sealwax_domain::model::AccountId(1),
            name_raw: "INBOX".into(),
            delimiter: Some('/'),
            role: FolderRole::Inbox,
            selectable: true,
            uidvalidity: Some(7),
            uidnext: 10,
            highestmodseq: 0,
            state: SyncState::Ready,
            floor: Some(1),
            below: 0,
            messages: 5,
        }
    }

    fn rows(uids: &[u32]) -> Vec<NewMessage> {
        uids.iter()
            .map(|uid| NewMessage {
                uid: Some(*uid),
                ..NewMessage::default()
            })
            .collect()
    }

    #[tokio::test]
    async fn gmail_says_which_of_the_inbox_rows_are_in_its_primary_tab() {
        let ca = test_ca();
        let config = ca.server.clone();
        let (listener, port) = bind().await;
        let server = tokio::spawn(async move {
            let mut peer = gmail(&listener, config).await;
            // Gmail answers X-GM-RAW with a plain SEARCH, never ESEARCH unless asked.
            let tag = peer
                .expect("UID SEARCH UID 3:5,9 X-GM-RAW \"category:primary\"")
                .await;
            peer.send(&format!(
                "* SEARCH 3 9\r\n{tag} OK SEARCH completed (Success)\r\n"
            ))
            .await;
            let tag = peer.expect("UID SEARCH UID 12 X-GM-RAW").await;
            peer.send(&format!(
                "* SEARCH\r\n{tag} OK SEARCH completed (Success)\r\n"
            ))
            .await;
            let tag = peer.expect("UID SEARCH UID 13 X-GM-RAW").await;
            peer.send(&format!("{tag} BAD Could not parse command\r\n"))
                .await;
            peer.drain().await
        });
        let mut conn = client(port, &ca.trust).await;
        let mut view = View {
            categories: categorizes(&conn, &inbox()),
            ..View::default()
        };
        assert!(view.categories, "Gmail's INBOX");
        let mut sent = FolderSync {
            role: FolderRole::Sent,
            ..inbox()
        };
        assert!(!categorizes(&conn, &sent), "only the INBOX");
        sent.role = FolderRole::Inbox;

        let mut found = rows(&[3, 4, 5, 9]);
        let tab = primary(&mut conn, &[3, 4, 5, 9], &mut view).await.unwrap();
        categorize(&mut found, tab.as_ref());
        let sides: Vec<Option<Split>> = found.iter().map(|row| row.category).collect();
        assert_eq!(
            sides,
            [
                Some(Split::Important),
                Some(Split::Other),
                Some(Split::Other),
                Some(Split::Important)
            ]
        );
        // An empty answer: Gmail put it in another tab.
        let tab = primary(&mut conn, &[12], &mut view).await.unwrap();
        assert_eq!(tab, Some(HashSet::new()));
        // Refused: no category, and this connection stops asking.
        let tab = primary(&mut conn, &[13], &mut view).await.unwrap();
        assert_eq!(tab, None);
        assert!(!view.categories && conn.categories.refused);
        assert!(!categorizes(&conn, &sent));
        let mut kept = rows(&[14]);
        let tab = primary(&mut conn, &[14], &mut view).await.unwrap();
        categorize(&mut kept, tab.as_ref());
        assert_eq!(kept[0].category, None, "the header rules sort it");
        drop(conn);
        let received = server.await.unwrap();
        assert_eq!(received.matches("X-GM-RAW").count(), 3, "{received}");
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn the_view_follows_exists_expunge_and_vanished() {
        let mut view = View {
            exists: 10,
            ..View::default()
        };
        view.track(&Response::MailboxData(MailboxDatum::Exists(12)));
        view.track(&Response::Expunge(3));
        view.track(&Response::Vanished {
            earlier: false,
            uids: vec![4..=5],
        });
        view.track(&Response::Vanished {
            earlier: true,
            uids: vec![1..=1, 20..=4_000_000_000],
        });
        assert_eq!(view.exists, 9);
        assert!(view.has_vanished(5) && view.has_vanished(1) && view.has_vanished(3_000_000));
        assert!(!view.has_vanished(7));
        view.track(&Response::Vanished {
            earlier: false,
            uids: vec![1..=u32::MAX],
        });
        assert_eq!(view.exists, 0);
    }

    #[test]
    fn search_dates() {
        assert_eq!(imap_date(0), "1-Jan-1970");
        assert_eq!(imap_date(1_767_254_820), "1-Jan-2026");
        assert_eq!(imap_date(1_709_251_199), "29-Feb-2024");
        assert_eq!(imap_date(1_791_158_400), "5-Oct-2026");
        assert_eq!(imap_date(-5), "1-Jan-1970");
    }
}
