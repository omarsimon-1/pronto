//! Subscription-backed CLI text generation. No keys, shell interpolation, or model calls in tests.
//! Detection never executes Omarchy's install-on-demand wrappers. Unknown capabilities fail closed.
use std::io::{self, Read, Write};
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);
impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
#[derive(Clone, Debug)]
pub struct Agent {
    pub name: String,
    pub path: PathBuf,
    pub available: bool,
    pub model: bool,
    pub models: Vec<String>,
    pub reason: String,
    args: Vec<String>,
    file_prompt: bool,
}
struct Spec {
    name: &'static str,
    args: &'static [&'static str],
    required: &'static [&'static str],
    file_prompt: bool,
}
// One entry contains the invocation and the capabilities the installed version must advertise.
const SPECS: &[Spec] = &[
    Spec {
        name: "claude",
        args: &[
            "-p",
            "--output-format",
            "text",
            "--tools",
            "",
            "--strict-mcp-config",
            "--safe-mode",
            "--no-session-persistence",
        ],
        required: &[
            "--tools",
            "--safe-mode",
            "--strict-mcp-config",
            "--no-session-persistence",
        ],
        file_prompt: false,
    },
    Spec {
        name: "codex",
        args: &[],
        required: &[],
        file_prompt: false,
    },
    Spec {
        name: "grok",
        args: &[
            "--tools",
            "",
            "--deny",
            "*",
            "--no-subagents",
            "--disable-web-search",
            "--output-format",
            "plain",
            "--prompt-file",
        ],
        required: &[
            "--tools",
            "--deny",
            "--no-subagents",
            "--disable-web-search",
            "--prompt-file",
        ],
        file_prompt: true,
    },
    Spec {
        name: "pi",
        args: &[
            "--print",
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-session",
        ],
        required: &[
            "--print",
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-session",
        ],
        file_prompt: false,
    },
    Spec {
        name: "omp",
        args: &[],
        required: &[],
        file_prompt: false,
    },
    Spec {
        name: "gemini",
        args: &[],
        required: &[],
        file_prompt: false,
    },
    Spec {
        name: "opencode",
        args: &[],
        required: &[],
        file_prompt: false,
    },
    Spec {
        name: "cursor-agent",
        args: &[],
        required: &[],
        file_prompt: false,
    },
    Spec {
        name: "openclaw",
        args: &[],
        required: &[],
        file_prompt: false,
    },
    Spec {
        name: "hermes",
        args: &[],
        required: &[],
        file_prompt: false,
    },
    Spec {
        name: "copilot",
        args: &[],
        required: &[],
        file_prompt: false,
    },
    Spec {
        name: "crush",
        args: &[],
        required: &[],
        file_prompt: false,
    },
    Spec {
        name: "muse",
        args: &[],
        required: &[],
        file_prompt: false,
    },
];

fn installed(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        let path = dir.join(name);
        let meta = path.metadata().ok()?;
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
            return None;
        }
        // Read a bounded prefix: binaries can be hundreds of MB.
        let mut prefix = Vec::new();
        std::fs::File::open(&path)
            .ok()?
            .take(4096)
            .read_to_end(&mut prefix)
            .ok()?;
        let prefix = String::from_utf8_lossy(&prefix);
        if prefix.contains("mise use -g") || prefix.contains("omarchy-install-") {
            return None;
        }
        Some(path)
    })
}

pub fn detect(cancel: &Cancel) -> Vec<Agent> {
    let mut agents = Vec::new();
    for spec in SPECS {
        if cancel.cancelled() {
            break;
        }
        let Some(path) = installed(spec.name) else {
            continue;
        };
        let Ok(help) = capture(
            &path,
            &["--help".into()],
            "",
            None,
            cancel,
            Duration::from_secs(8),
        ) else {
            continue;
        };
        let available =
            !spec.args.is_empty() && spec.required.iter().all(|flag| help.contains(flag));
        agents.push(Agent {
            name: spec.name.into(),
            path,
            available,
            model: help.contains("--model"),
            models: vec![],
            reason: if available {
                "Uses the CLI’s existing sign-in"
            } else {
                "Installed; this version has no verified no-tools invocation"
            }
            .into(),
            args: spec.args.iter().map(|s| (*s).into()).collect(),
            file_prompt: spec.file_prompt,
        });
    }
    // New Omarchy entries are detected, but cannot execute without a reviewed table entry.
    if let Ok(script) = std::fs::read_to_string("/usr/bin/omarchy-default-agent")
        && let Some(names) = script.lines().find_map(|line| {
            line.strip_prefix("# omarchy:args=[")
                .and_then(|s| s.strip_suffix(']'))
        })
    {
        for name in names.split('|') {
            if agents.iter().any(|a| a.name == name) || SPECS.iter().any(|s| s.name == name) {
                continue;
            }
            if let Some(path) = installed(name) {
                agents.push(Agent {
                    name: name.into(),
                    path,
                    available: false,
                    model: false,
                    models: vec![],
                    reason: "Needs a verified no-tools adapter".into(),
                    args: vec![],
                    file_prompt: false,
                });
            }
        }
    }
    agents
}

pub fn default_agent(agents: &[Agent]) -> Option<String> {
    let path = std::env::home_dir()?.join(".config/omarchy/defaults/agent");
    if let Ok(name) = std::fs::read_to_string(path) {
        return Some(name.trim().into());
    }
    agents.first().map(|a| a.name.clone())
}

impl Agent {
    pub fn list_models(&self, cancel: &Cancel) -> Vec<String> {
        let args: &[&str] = match self.name.as_str() {
            "grok" => &["models"],
            "codex" => &["debug", "models", "--bundled"],
            _ => return vec![],
        };
        let Ok(text) = capture(
            &self.path,
            &args.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
            "",
            None,
            cancel,
            Duration::from_secs(10),
        ) else {
            return vec![];
        };
        if self.name == "codex"
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
        {
            return value
                .get("models")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|m| m.get("slug").and_then(|s| s.as_str()).map(str::to_owned))
                .collect();
        }
        text.lines()
            .filter_map(|line| {
                line.split_whitespace()
                    .find(|s| s.starts_with("grok-"))
                    .map(str::to_owned)
            })
            .take(100)
            .collect()
    }
    pub fn generate(&self, model: &str, prompt: &str, cancel: &Cancel) -> Result<String, String> {
        if !self.available {
            return Err(self.reason.clone());
        }
        let dir = tempfile::tempdir().map_err(|_| "Cannot create private AI workspace")?;
        let mut args = self.args.clone();
        if self.file_prompt {
            let path = dir.path().join("prompt.txt");
            crate::accounts::replace(&path, prompt).map_err(|_| "Cannot write AI prompt")?;
            args.push(path.to_string_lossy().into_owned());
        }
        if self.model && !model.trim().is_empty() {
            args.extend(["--model".into(), model.trim().into()]);
        }
        capture(
            &self.path,
            &args,
            if self.file_prompt { "" } else { prompt },
            Some(dir.path()),
            cancel,
            Duration::from_secs(120),
        )
    }
}

/// Both pipes are drained concurrently; prompt writes cannot defeat timeout/cancellation.
/// A process group kills CLI descendants too. Mail and provider stderr never enter app logs.
fn capture(
    path: &Path,
    args: &[String],
    input: &str,
    cwd: Option<&Path>,
    cancel: &Cancel,
    timeout: Duration,
) -> Result<String, String> {
    if cancel.cancelled() {
        return Err("Cancelled".into());
    }
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    for key in [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "XAI_API_KEY",
    ] {
        command.env_remove(key);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Cannot start the signed-in agent CLI")?;
    let pid = child.id() as i32;
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_owned();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let started = Instant::now();
    let status = loop {
        if cancel.cancelled() || started.elapsed() >= timeout {
            break Err("Agent cancelled or timed out".to_owned());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err("Agent failed; check its sign-in in a terminal".into())
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => break Err("Cannot wait for agent".into()),
        }
    };
    // SAFETY: the child starts its own process group; only that owned group is signalled.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.wait();
    let _ = writer.join();
    let bytes = reader
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("reader stopped")))
        .map_err(|_| "Cannot read agent output")?;
    status?;
    if bytes.len() > 1024 * 1024 {
        return Err("Agent response exceeded the size limit".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "Agent returned invalid text")?;
    if text.trim().is_empty() {
        return Err("Agent returned an empty response".into());
    }
    Ok(text.trim().to_owned())
}

impl Agent {
    /// Self-test seam: a local shell script, never discovered from settings or environment.
    #[doc(hidden)]
    pub fn test_fixture(path: PathBuf) -> Self {
        Self {
            name: "fixture".into(),
            path,
            available: true,
            model: true,
            models: vec![],
            reason: "Synthetic self-test agent".into(),
            args: vec![],
            file_prompt: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fake(body: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        (dir, path)
    }
    #[test]
    fn fake_agent_uses_stdin_and_literal_arguments() {
        let (_dir, path) = fake("cat >/dev/null; printf '%s' \"$1\"");
        assert_eq!(
            capture(
                &path,
                &["$(touch should-not-exist)".into()],
                "private mail",
                None,
                &Cancel::default(),
                Duration::from_secs(2)
            )
            .unwrap(),
            "$(touch should-not-exist)"
        );
    }
    #[test]
    fn timeout_cancels_descendants_and_blocked_stdin() {
        let (_dir, path) = fake("sleep 30");
        let started = Instant::now();
        assert!(
            capture(
                &path,
                &[],
                &"x".repeat(1_000_000),
                None,
                &Cancel::default(),
                Duration::from_millis(100)
            )
            .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn cancellation_and_failure_do_not_return_mail_or_stderr() {
        let (_dir, path) = fake("echo secret >&2; exit 1");
        let cancel = Cancel::default();
        assert!(
            !capture(&path, &[], "", None, &cancel, Duration::from_secs(2))
                .unwrap_err()
                .contains("secret")
        );
        cancel.cancel();
        assert!(capture(&path, &[], "", None, &cancel, Duration::from_secs(2)).is_err());
    }
}
