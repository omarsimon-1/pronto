use super::*;
use crate::imap::{Credentials, connect};
use crate::net::testing::{Peer, bind, test_ca};
use sealwax_domain::account::{Endpoint, Security};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

fn options(limit: usize) -> SampleOptions {
    SampleOptions {
        since: fetch::internal_date("1-Oct-2026 00:00:00 +0000").unwrap(),
        limit,
        refresh: false,
        checkpoints: Vec::new(),
        own_addresses: vec!["me@example.test".into()],
    }
}

fn candidate(uid: u32, recipient: usize) -> Candidate {
    Candidate {
        folder: 0,
        uid,
        date: i64::from(uid),
        size: [1_000, 5_000, 20_000][uid as usize % 3],
        recipients: vec![format!("person{recipient}@domain{}.test", recipient % 4)],
        domain: format!("domain{}.test", recipient % 4),
    }
}

#[test]
fn selection_is_stable_and_balances_domains_sizes_dates_and_recipients() {
    let make = || {
        (1..=5_000)
            .map(|uid| candidate(uid, uid as usize % 100))
            .collect()
    };
    let selected = select(make(), 300);
    assert_eq!(selected.len(), 300);
    assert_eq!(
        selected
            .iter()
            .map(|candidate| candidate.uid)
            .collect::<Vec<_>>(),
        select(make(), 300)
            .iter()
            .map(|candidate| candidate.uid)
            .collect::<Vec<_>>()
    );
    let mut recipients = HashMap::new();
    let mut domains = HashMap::new();
    let mut dates = [0; 4];
    let mut sizes = [0; 3];
    for candidate in selected {
        for recipient in candidate.recipients {
            *recipients.entry(recipient).or_insert(0) += 1;
        }
        *domains.entry(candidate.domain).or_insert(0) += 1;
        dates[(candidate.uid as usize - 1) * 4 / 5_000] += 1;
        sizes[size_bucket(candidate.size)] += 1;
    }
    assert!(recipients.values().all(|count| *count <= 15));
    assert!(domains.values().all(|count| *count == 75));
    assert!(dates.iter().all(|count| *count >= 70), "{dates:?}");
    assert!(sizes.iter().all(|count| *count >= 90), "{sizes:?}");
    assert_eq!(
        select((1..=100).map(|uid| candidate(uid, 0)).collect(), 300).len(),
        15
    );
    assert_eq!(
        select((1..=100).map(|uid| candidate(uid, 0)).collect(), 50).len(),
        5
    );
    let mut reverse = make();
    reverse.reverse();
    assert_eq!(
        select(make(), 300)
            .iter()
            .map(|c| c.uid)
            .collect::<Vec<_>>(),
        select(reverse, 300)
            .iter()
            .map(|c| c.uid)
            .collect::<Vec<_>>()
    );
}

#[test]
fn every_recipient_in_a_group_is_capped() {
    let mut candidates: Vec<_> = (1..=100).map(|uid| candidate(uid, uid as usize)).collect();
    for candidate in &mut candidates {
        candidate.recipients.push("same@example.test".into());
    }
    assert_eq!(select(candidates, 300).len(), 15);
}

#[test]
fn headers_reject_forward_calendar_automation_and_other_senders_before_bodies() {
    let own = ["me@example.test".into()];
    let header = |extra: &str| {
        format!("From: Me <ME@example.test>\r\nTo: person@example.test\r\n{extra}\r\n")
    };
    assert!(
        eligible_header(
            header("Subject: Re: Plans\r\nAuto-Submitted: no\r\n").as_bytes(),
            &own
        )
        .is_some()
    );
    for extra in [
        "Subject: Fwd: document\r\n",
        "Subject: Re: Fw: document\r\n",
        "Subject: Accepted: meeting\r\n",
        "Subject: Declined: meeting\r\n",
        "Subject: Tentative: meeting\r\n",
        "Subject: Out of office\r\n",
        "Subject: Automatic reply: hello\r\n",
        "Auto-Submitted: auto-replied\r\n",
        "X-Autoreply: yes\r\n",
        "X-Autorespond: yes\r\n",
        "Content-Type: text/calendar; method=REPLY\r\n",
    ] {
        assert!(
            eligible_header(header(extra).as_bytes(), &own).is_none(),
            "{extra}"
        );
    }
    assert!(
        eligible_header(
            b"From: other@example.test\r\nTo: person@example.test\r\n\r\n",
            &own
        )
        .is_none()
    );
}

fn parse_set(set: &str) -> Vec<u32> {
    set.split(',')
        .flat_map(|part| match part.split_once(':') {
            Some((start, end)) => start.parse::<u32>().unwrap()..=end.parse::<u32>().unwrap(),
            None => {
                let uid = part.parse::<u32>().unwrap();
                uid..=uid
            }
        })
        .collect()
}

struct Fake {
    count: Arc<AtomicU32>,
    validity: Arc<AtomicU32>,
    server: tokio::task::JoinHandle<(Vec<u32>, Vec<u32>, Vec<String>)>,
    conn: Connection,
}

impl Fake {
    async fn new(count: u32) -> Self {
        let ca = test_ca();
        let config = ca.server.clone();
        let (listener, port) = bind().await;
        let count = Arc::new(AtomicU32::new(count));
        let validity = Arc::new(AtomicU32::new(777));
        let current = count.clone();
        let current_validity = validity.clone();
        let server = tokio::spawn(async move {
            let mut peer = Peer::accept_tls(&listener, config).await;
            peer.send("* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready\r\n")
                .await;
            let tag = peer.expect("AUTHENTICATE PLAIN").await;
            peer.send("+ \r\n").await;
            peer.line().await;
            peer.send(&format!("{tag} OK authenticated\r\n")).await;
            let tag = peer.expect("CAPABILITY").await;
            peer.send(&format!(
                "* CAPABILITY IMAP4rev1 LIST-STATUS\r\n{tag} OK done\r\n"
            ))
            .await;
            let mut bodies = Vec::new();
            let mut headers = Vec::new();
            let mut searches = Vec::new();
            while let Some(line) = peer.line().await {
                let (tag, command) = line.split_once(' ').unwrap();
                let count = current.load(Ordering::SeqCst);
                let validity = current_validity.load(Ordering::SeqCst);
                if command.starts_with("LIST ") {
                    peer.send("* LIST (\\Sent) \"/\" \"Sent\"\r\n").await;
                } else if command.starts_with("EXAMINE ") {
                    assert_eq!(command, "EXAMINE \"Sent\"");
                    peer.send(&format!("* {count} EXISTS\r\n* OK [UIDVALIDITY {validity}] ok\r\n* OK [UIDNEXT {}] ok\r\n", count + 1)).await;
                } else if command.starts_with("UID SEARCH ") {
                    assert!(command.contains("SINCE "));
                    assert!(!command.contains("ALL"));
                    searches.push(command.to_owned());
                    let start = command
                        .split(" UID ")
                        .nth(1)
                        .map(|set| set.split(':').next().unwrap().parse::<u32>().unwrap())
                        .unwrap_or(1);
                    let found = (start..=count)
                        .map(|uid| uid.to_string())
                        .collect::<Vec<_>>()
                        .join(" ");
                    peer.send(&format!("* SEARCH {found}\r\n")).await;
                } else if command.starts_with("UID FETCH ") {
                    let set = command.split_whitespace().nth(2).unwrap();
                    for uid in parse_set(set) {
                        if command.contains("HEADER.FIELDS") {
                            assert!(!command.contains("BODY.PEEK[TEXT]"));
                            assert!(!command.contains("BODY.PEEK[]"));
                            headers.push(uid);
                            let header = format!(
                                "From: me@example.test\r\nTo: person{}@domain{}.test\r\nSubject: Planning {uid}\r\nContent-Type: text/plain\r\n\r\n",
                                uid % 100,
                                uid % 4
                            );
                            let date = if uid <= 5_000 {
                                "30-Oct-2026 12:00:00 +0000"
                            } else {
                                "31-Oct-2026 12:00:00 +0000"
                            };
                            let size = [1_000, 5_000, 20_000][uid as usize % 3];
                            peer.send(&format!("* {uid} FETCH (UID {uid} INTERNALDATE \"{date}\" RFC822.SIZE {size} BODYSTRUCTURE (\"TEXT\" \"PLAIN\" NIL NIL NIL \"7BIT\" 300 4) BODY[HEADER.FIELDS ({FIELDS})] {{{}}}\r\n{header})\r\n", header.len())).await;
                        } else {
                            assert!(command.ends_with("(UID BODY.PEEK[])"));
                            bodies.push(uid);
                            let raw = format!(
                                "From: me@example.test\r\nTo: person@example.test\r\n\r\nPRIVATE-BODY-{uid} Here are the details of the plan we discussed yesterday and the next steps for our project this week."
                            );
                            peer.send(&format!(
                                "* {uid} FETCH (UID {uid} BODY[] {{{}}}\r\n{raw})\r\n",
                                raw.len()
                            ))
                            .await;
                        }
                    }
                } else {
                    panic!("unexpected command: {command}");
                }
                peer.send(&format!("{tag} OK done\r\n")).await;
            }
            (headers, bodies, searches)
        });
        let conn = connect(
            &Endpoint {
                host: "127.0.0.1".into(),
                port,
                security: Security::Tls,
                certificate: None,
            },
            Credentials::Password {
                username: "user",
                password: "secret",
            },
            ca.trust.client_config().unwrap(),
        )
        .await
        .unwrap();
        Self {
            count,
            validity,
            server,
            conn,
        }
    }
}

#[tokio::test]
async fn five_thousand_messages_fetch_only_three_hundred_bodies_then_fifty_new() {
    let mut fake = Fake::new(5_000).await;
    let mut options = options(300);
    let sampled = sample(&mut fake.conn, &options, |_, _| {}).await.unwrap();
    assert_eq!(sampled.fetched, 300);
    assert_eq!(sampled.originals.len(), 300);
    assert_eq!(sampled.checkpoints[0].last_uid, 5_000);
    assert!(!format!("{sampled:?}").contains("PRIVATE-BODY"));
    options.refresh = true;
    options.limit = 1_000;
    options.checkpoints = sampled.checkpoints;
    drop(sampled.originals);
    fake.count.store(5_100, Ordering::SeqCst);
    let refreshed = sample(&mut fake.conn, &options, |_, _| {}).await.unwrap();
    assert_eq!(refreshed.fetched, 50);
    assert_eq!(refreshed.checkpoints[0].last_uid, 5_100);
    options.checkpoints = refreshed.checkpoints;
    let empty = sample(&mut fake.conn, &options, |_, _| {}).await.unwrap();
    assert!(empty.originals.is_empty());
    drop(fake.conn);
    let (headers, bodies, searches) = fake.server.await.unwrap();
    assert_eq!(headers.len(), 5_100);
    assert_eq!(bodies.len(), 350);
    assert!(bodies[300..].iter().all(|uid| *uid > 5_000));
    assert!(searches[1].contains("UID 5001:*"));
    assert!(searches[2].contains("UID 5101:*"));
}

#[tokio::test]
async fn uidvalidity_reset_uses_date_instead_of_relearning_old_messages() {
    let mut fake = Fake::new(5_020).await;
    fake.validity.store(888, Ordering::SeqCst);
    let mut options = options(50);
    options.refresh = true;
    options.checkpoints = vec![FolderCheckpoint {
        name_raw: "Sent".into(),
        uidvalidity: 777,
        last_uid: 5_000,
        last_date: fetch::internal_date("30-Oct-2026 12:00:00 +0000").unwrap(),
    }];
    let sampled = sample(&mut fake.conn, &options, |_, _| {}).await.unwrap();
    assert_eq!(sampled.fetched, 20);
    assert_eq!(sampled.checkpoints[0].uidvalidity, 888);
    drop(fake.conn);
    let (_, bodies, searches) = fake.server.await.unwrap();
    assert!(bodies.iter().all(|uid| *uid > 5_000));
    assert!(!searches[0].contains(" UID "));
}

#[tokio::test]
async fn cancellation_is_shared_and_remembered_before_waiting() {
    let cancel = Cancel::default();
    cancel.clone().cancel();
    assert!(cancel.is_cancelled());
    tokio::time::timeout(std::time::Duration::from_millis(50), cancel.cancelled())
        .await
        .unwrap();
}
