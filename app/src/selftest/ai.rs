//! AI integration uses a shell-script CLI and fictional messages only.
use super::{Harness, StepResult, equal, number};
use crate::ai::Controller;
use gtk4::glib;
use sealwax_domain::model::{AccountId, FolderId, FolderRole, MessageId};
use std::{cell::RefCell, collections::HashMap, os::unix::fs::PermissionsExt, rc::Rc};
thread_local! { static MAIL: RefCell<HashMap<String, (AccountId, FolderId, MessageId)>> = RefCell::default(); }
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("ai-fixture", 1, 1),
    ("ai-mail", 2, 2),
    ("ai-toggle", 2, 2),
    ("ai-arrive", 1, 1),
    ("ai-unread", 0, 0),
    ("ai-resume", 0, 0),
    ("ai-refresh", 0, 0),
    ("ai-retry", 0, 0),
    ("ai-open", 1, 1),
    ("ai-cap", 1, 1),
    ("assert-ai", 2, 2),
    ("assert-ai-calls", 1, 1),
];
pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}
fn controller(harness: &Harness) -> Result<Rc<Controller>, String> {
    harness
        .state
        .ai()
        .ok_or_else(|| "AI not initialized".into())
}
pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let this = controller(harness)?;
    match command {
        "ai-fixture" => {
            let path = harness.root.join("fake-agent.sh");
            let response = match args[0].as_str() {
                "ok" => {
                    "cat > \"$0.prompt\"\nif grep -q 'writing.style' \"$0.prompt\"; then printf '# Writing style\\nUse short sentences. Greet with Hi. Close with Thanks.\\nExample: Hi [name], thanks for the update.\\n'; if grep -q 'Keep custom preference' \"$0.prompt\"; then printf 'Keep custom preference.\\n'; fi; else printf 'Thanks for the update. Could you share the next steps?\\n'; fi"
                }
                "fail" => "cat >/dev/null; exit 1",
                "slow" => "sleep 30; echo should-not-be-saved",
                _ => return Err("unknown fixture mode".into()),
            };
            std::fs::write(
                &path,
                format!("#!/bin/sh\nprintf 'call\\n' >> \"$0.calls\"\n{response}\n"),
            )
            .map_err(|e| e.to_string())?;
            std::fs::write(harness.root.join("fake-agent.sh.calls"), "")
                .map_err(|e| e.to_string())?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
            this.test_agent(path).await
        }
        "ai-mail" => add_mail(harness, &args[0], &args[1]).await,
        "ai-toggle" => {
            let mut options = this.options();
            let on = super::script::boolean(&args[1])?;
            match args[0].as_str() {
                "auto" => options.auto = on,
                "style" => options.style = on,
                _ => return Err("unknown AI toggle".into()),
            }
            this.set_options(options);
            Ok(())
        }
        "ai-cap" => {
            let mut options = this.options();
            options.daily_cap = number(&args[0])? as u32;
            this.set_options(options);
            Ok(())
        }
        "ai-arrive" => {
            let (account, folder, message) = MAIL
                .with(|m| m.borrow().get(&args[0]).copied())
                .ok_or("missing AI mail")?;
            this.test_event(account, folder, message);
            Ok(())
        }
        "ai-unread" => {
            this.unread_now();
            Ok(())
        }
        "ai-open" => {
            let (_, _, message) = MAIL
                .with(|m| m.borrow().get(&args[0]).copied())
                .ok_or("missing AI mail")?;
            equal("opens AI draft", this.open_ready(message), true)
        }
        "ai-resume" => this.test_resume().await,
        "ai-refresh" => {
            let account = harness
                .state
                .mailbox()
                .accounts()
                .first()
                .map(|account| account.id)
                .ok_or("No account")?;
            this.refresh(account);
            Ok(())
        }
        "ai-retry" => {
            this.test_retry_now().await;
            Ok(())
        }
        "assert-ai" => equal(&args[0], this.test_count(&args[0])?, number(&args[1])?),
        "assert-ai-calls" => {
            let calls = std::fs::read_to_string(harness.root.join("fake-agent.sh.calls"))
                .map_err(|e| e.to_string())?;
            equal("agent calls", calls.lines().count(), number(&args[0])?)
        }
        _ => Err("Unknown AI command".into()),
    }
}
async fn add_mail(harness: &Harness, name: &str, kind: &str) -> StepResult {
    use sealwax_infrastructure::ingest::{self, RawMessage};
    let mailbox = harness.state.mailbox();
    let account = mailbox.accounts().first().cloned().ok_or("No account")?;
    let role = if kind == "sent" {
        FolderRole::Sent
    } else {
        FolderRole::Inbox
    };
    let folder = mailbox
        .folders()
        .into_iter()
        .find(|f| f.account == account.id && f.role == role)
        .ok_or("Missing folder")?
        .id;
    let sender = match kind {
        "own" | "sent" => account.address.as_str(),
        "noreply" => "noreply@example.test",
        _ => "ada@example.test",
    };
    let header = if kind == "bulk" {
        "List-Unsubscribe: <https://example.test/unsubscribe>\r\n"
    } else {
        ""
    };
    let date = glib::DateTime::now_utc()
        .unwrap()
        .format("%a, %d %b %Y %H:%M:%S +0000")
        .unwrap();
    let raw = format!("From: {sender}\r\nTo: {}\r\nSubject: AI test {name}\r\nMessage-ID: <ai-{name}@example.test>\r\nDate: {date}\r\n{header}\r\nHi there,\r\nNotes for {name}. Could you review the proposal and share your thoughts on the next steps before our meeting tomorrow?\r\nThanks,\r\nAda Example\r\n-- \r\nPrivate signature\r\n\r\nOn Tuesday, Bob wrote:\r\n> Quoted history\r\n", account.address).into_bytes();
    let store = mailbox.shared_store().ok_or("No store")?;
    let blobs = mailbox.blobs().ok_or("No blobs")?;
    let result = super::blocking(move || {
        ingest::ingest_raw(
            &store,
            &blobs,
            vec![RawMessage {
                folder,
                raw,
                ..RawMessage::default()
            }],
        )
        .map_err(|e| e.to_string())
    })
    .await??;
    let id = result
        .ids
        .first()
        .copied()
        .flatten()
        .ok_or("Mail not ingested")?;
    MAIL.with(|m| m.borrow_mut().insert(name.into(), (account.id, folder, id)));
    super::mail::settled(harness).await
}
