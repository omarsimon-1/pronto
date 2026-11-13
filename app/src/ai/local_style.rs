//! Metadata-first sampling of already-local Sent mail, including POP3's local Sent folder.
use super::*;
use sealwax_domain::{model::BodyKind, split::HeaderFacts};
use sealwax_engine::{
    api::Original,
    sent_sample::{Candidate, FolderCheckpoint, SampleOptions, SentSample, select},
};

impl Controller {
    /// Read metadata throughout the recent window, then open only the selected existing
    /// blobs. This path never asks the engine to download, evict, or change local mail.
    pub(super) async fn local_style_sample(
        &self,
        state: &AppState,
        store: &Store,
        account: &Account,
        options: SampleOptions,
        cancel: &Cancel,
    ) -> Result<SentSample, String> {
        let mut result = SentSample::default();
        let mut candidates = Vec::new();
        for (folder_index, folder) in state
            .mailbox()
            .folders()
            .into_iter()
            .filter(|folder| folder.account == account.id && folder.role == FolderRole::Sent)
            .enumerate()
        {
            let name_raw = format!("local:{}", folder.id.0);
            let previous = options
                .checkpoints
                .iter()
                .find(|checkpoint| checkpoint.name_raw == name_raw && checkpoint.uidvalidity == 1);
            let last_uid = previous.map_or(0, |checkpoint| checkpoint.last_uid);
            let mut checkpoint = FolderCheckpoint {
                name_raw,
                uidvalidity: 1,
                last_uid,
                last_date: previous.map_or(options.since, |checkpoint| checkpoint.last_date),
            };
            let mut offset = 0;
            'pages: loop {
                if cancel.cancelled() {
                    return Err("Style learning paused".into());
                }
                let page = store
                    .page(ViewSpec::folder(folder.id), offset, 500)
                    .recv()
                    .await
                    .map_err(|_| "Cannot read local Sent metadata")?;
                let page_len = page.rows.len();
                for row in page.rows {
                    if cancel.cancelled() {
                        return Err("Style learning paused".into());
                    }
                    // Message views are ordered by date descending, so the remaining pages
                    // cannot contain anything inside the requested recent window.
                    if row.date < options.since {
                        break 'pages;
                    }
                    let uid = u32::try_from(row.id.0)
                        .map_err(|_| "Local Sent message IDs cannot be checkpointed")?;
                    checkpoint.last_uid = checkpoint.last_uid.max(uid);
                    checkpoint.last_date = checkpoint.last_date.max(row.date);
                    if options.refresh && uid <= last_uid {
                        continue;
                    }
                    let Some(detail) = store
                        .message(row.id)
                        .recv()
                        .await
                        .map_err(|_| "Cannot read local Sent metadata")?
                        .value
                    else {
                        continue;
                    };
                    if detail.blob.is_none()
                        || detail.size < 100
                        || detail.size > 256 * 1024
                        || detail.body_kind == BodyKind::None
                        || detail.headers.facts.contains(HeaderFacts::AUTO_SUBMITTED)
                        || !local_subject_eligible(&detail.headers.subject)
                        || !detail.headers.from.iter().any(|sender| {
                            options
                                .own_addresses
                                .iter()
                                .any(|own| own.trim().eq_ignore_ascii_case(sender.email.trim()))
                        })
                        || detail.attachments.iter().any(|attachment| {
                            matches!(
                                attachment.mime_type.as_str(),
                                "text/calendar" | "message/rfc822"
                            ) || attachment
                                .name
                                .as_ref()
                                .is_some_and(|name| name.to_ascii_lowercase().ends_with(".ics"))
                        })
                    {
                        continue;
                    }
                    let mut recipients = detail
                        .headers
                        .to
                        .iter()
                        .chain(&detail.headers.cc)
                        .map(|address| address.email.trim().to_ascii_lowercase())
                        .filter(|address| address.contains('@'))
                        .collect::<Vec<_>>();
                    recipients.sort();
                    recipients.dedup();
                    let Some(domain) = recipients
                        .first()
                        .and_then(|address| address.rsplit_once('@'))
                        .map(|(_, domain)| domain.to_owned())
                    else {
                        continue;
                    };
                    candidates.push(Candidate {
                        folder: folder_index,
                        uid,
                        date: row.date,
                        size: detail.size as u32,
                        recipients,
                        domain,
                    });
                }
                if page_len < 500 {
                    break;
                }
                offset += 500;
            }
            result.checkpoints.push(checkpoint);
        }
        for candidate in select(candidates, options.cap()) {
            if cancel.cancelled() {
                return Err("Style learning paused".into());
            }
            if let Some(raw) = store
                .raw_message(MessageId(i64::from(candidate.uid)))
                .recv()
                .await
                .map_err(|_| "Cannot read selected local Sent example")?
            {
                result.originals.push(Original(raw));
                result.fetched += 1;
            }
        }
        Ok(result)
    }
}

fn local_subject_eligible(subject: &str) -> bool {
    let subject = subject.trim().to_ascii_lowercase();
    let mut subject = subject.as_str();
    while let Some(rest) = subject.strip_prefix("re:") {
        subject = rest.trim_start();
    }
    ![
        "fw:",
        "fwd:",
        "accepted:",
        "declined:",
        "tentative:",
        "canceled:",
        "cancelled:",
        "automatic reply",
        "auto reply",
        "auto-reply",
        "autoreply",
        "ooo:",
    ]
    .iter()
    .any(|prefix| subject.starts_with(prefix))
        && !subject.contains("out of office")
        && !subject.contains("out-of-office")
        && subject != "ooo"
}
