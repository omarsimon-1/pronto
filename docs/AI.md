# AI drafts and writing-style sheets

Open **Settings → AI** (Preferences in the app menu). Both features start off.
The agent picker uses **Omarchy default agent**, from
`~/.config/omarchy/defaults/agent`; without that file it picks the first installed
CLI. Sign in to the CLI in a terminal first. Pronto uses that sign-in and never
asks for or stores an API key. Both switches disclose that message contents go to
the chosen agent's provider.

Select a model, or leave the model field blank for the CLI's own default. Grok's
`models` command and Codex's bundled catalog populate a list when available; other
CLIs use free text. Listing models does not generate a response.

## CLI support

The adapter table is `infrastructure/src/agent.rs`. Detection checks executable
files and their actual `--help`, off the GTK main loop, without executing Omarchy
install-on-demand launchers. It runs once: at startup when a feature is already
on, otherwise the first time Settings → AI is opened. With both features off and
Settings unopened, Pronto runs no CLI at all. It recognizes Claude, Codex, Grok, Gemini, OpenCode,
Cursor Agent, Pi, OMP, OpenClaw, Hermes, Copilot, Crush, Muse, and additional names
in `/usr/bin/omarchy-default-agent`.

The installed Claude and Grok advertise the controls used here: headless plain
text, empty tool sets, and disabled external tools/customizations. Claude uses
`--safe-mode --tools '' --strict-mcp-config --no-session-persistence`; Grok uses
`--tools '' --deny '*' --no-subagents --disable-web-search --prompt-file`.
Prompts use stdin or a private temporary file, never shell interpolation or a
command-line mail argument. Calls time out after two minutes. Pausing, choosing
another agent or model, switching style matching, disabling a feature, or quitting
cancels the worker and kills its process group. Changing a cap or a sampling limit
does not interrupt work in flight, and no settings change resumes paused work:
only **Resume AI work** does. The app does not log prompts, responses, or provider
error text.

The worker has no timer while there is nothing to do: with both features off, or
with an empty queue and finished style sheets, it sleeps until mail arrives or a
setting changes. It checks once a second only while work is queued or a saved AI
draft is waiting to be opened, sent or discarded.

**Known limitation:** Codex 0.162.1 and the installed Hermes are detected but
unavailable for mail generation: their help does not establish an invocation that
removes every tool. Codex's `exec --sandbox read-only` still permits tools, so it
is insufficient for this feature's no-tools requirement. Other detected CLIs
without a verified adapter are also unavailable, with an explanation in Settings.
This is deliberate fail-closed behavior; adding support requires one reviewed
table entry with flags checked against that CLI's help. The
[official Codex configuration reference](https://developers.openai.com/codex/config-reference)
documents the scoped tool controls; it does not provide a global tool-off flag for
this installed version. No real model was called during development tests.

## Match my writing style

Enabling the switch starts a job per account, learning from **up to 300 recent
Sent emails from the last 12 months** by default. Pronto first fetches message
IDs, dates, recipients, sizes, and filtering headers, then selects a sample before
fetching any bodies. The style job never downloads the whole Sent folder. It uses
a stable selection across dates, message lengths, and recipient domains, with at
most 10% of the sample or 15 messages per recipient, whichever is smaller.

Forwards, calendar messages, automatic replies, attachment-only or near-empty
messages, and writing shorter than 15 words are skipped. Quoted replies and
signature blocks are removed before batching. Unsuitable messages can leave the
finished sample smaller than its cap. Each batch is bounded by message count and
text size; batch summaries are merged in bounded groups into the Markdown guide.

**Emails to sample**, **Recent months to consider**, and **New emails per refresh**
in Settings → AI override the defaults. **Use more emails** selects up to 1,000
emails from the last 24 months for the next build; use **Rebuild** to apply it to an
existing sheet. The refresh cap can be lowered from its maximum of 50 emails.
Progress reports the recent-email sample being processed.

The guide describes greetings, sign-offs, length, formality, punctuation,
capitalization, phrases, recipient differences, and about 20 short anonymized
example snippets.
It lives at `$XDG_DATA_HOME/pronto/style/<account-key>.md` (the usual XDG fallback
applies). Unsafe filename characters in an account key are percent encoded.
**View/Edit style sheet** opens it in the default Markdown editor. **Rebuild**
replaces it from a new bounded sample of Sent mail. **Refresh** updates the
existing sheet using only mail sent since that account's last run, at most 50
messages by default. A date/UID watermark is kept separately for each account.
Settings shows progress and the last-updated date.
Completed batch summaries are checkpointed. After a restart, those summaries finish
the sheet; unprocessed examples held in memory are discarded. A restart before the
first completed batch starts a new bounded sample.
**Pause AI work / Resume AI work** also keeps progress. Local sheets and checkpoints
are private files. Existing edited sheets are read fresh for each draft while the
style switch is enabled. Until the first sheet exists, drafts use the normal prompt.

Style sampling keeps the sheet, short snippets, intermediate summaries, and
per-account progress, not raw message bodies. Its body downloads are transient
and do not populate the normal mail cache. Cleaned examples are sent to the chosen
agent's provider, whose own retention policies still apply.

**Upgrading from an earlier version:** existing sheets keep working. Earlier
style builds downloaded bodies into Pronto's shared mail cache without marking
which downloads belonged to the style feature. Pronto preserves that cache
because deleting it could remove other locally stored mail, and shows a notice
in Settings → AI. To reduce the shared cache, lower `[storage] cache_size` in
`config.toml` (minimum `"100 MiB"`); ordinary cache eviction preserves POP3 mail and
drafts and only removes bodies that can be downloaded again. Set `prefetch_days = 0`
in the same section to stop background prefetching. Do not delete `store.sqlite`
or `blobs/` to clear style data. See [storage settings](USER_GUIDE.md#configtoml).

## Draft a reply to each new message

Enabling this switch records the start time. Only the sync engine's new-arrival
events after that boundary are queued; first syncs and older-history downloads do
not trigger drafts. **Draft for unread in Inbox now** explicitly includes existing
unread Inbox mail. It must be clicked with auto-drafting enabled, and queues at
most the 50 newest unread messages across the Inboxes (fewer when the daily cap is
lower).

The queue has one worker and defaults to 200 generation attempts per UTC day.
The cap includes failed attempts and retries; remaining work waits until the next
day. A failure (the CLI failing, a message that cannot be downloaded) keeps the
job, moves it to the back, and holds the whole queue for 30 seconds; each further
failure in a row doubles the wait, up to 30 minutes, so an outage costs one
attempt per wait and the queue survives it. A job is dropped after three failures
only when the agent answered other mail in between, which shows the message is the
problem; otherwise after eight. No-reply
senders, list/bulk/automated mail, the user's identities, answered messages, and
threads with a newer sent reply or an existing draft are skipped. The model can
also return `NO_REPLY` for a message that needs no response; the answer is
recognized in any case, quoted or with closing punctuation, but only when it is
the whole answer.

Prompts include recent thread context (bounded to fit a context window), the
account's reply identity/signature, and the style sheet when enabled. The
instructions come first. Each message, the signature and the style sheet sit
between tags ending in a random token made for that one prompt; mail cannot
contain the token, and any occurrence is removed, so a message cannot close its
section or open a new one. The instructions state that everything inside the
thread is untrusted data to reply to, never instructions to follow. This limits
prompt injection; it cannot rule out a model being misled, which is why the
agent runs without tools and only ever writes a draft. Output is
escaped as plain text, with blank lines kept as paragraph breaks; Pronto appends
the editable account signature once.
Replies are ordinary stored drafts attached to their source thread. **Draft ready**
appears in the list, and Reply opens the saved draft in the composer. A final check
on the store's writer protects against a user reply/draft created while the agent
was working. Unrelated store activity during that check (mail arriving in another
folder) no longer discards the finished draft: the check is repeated and the save
tried again. Drafts may sync to the server's Drafts folder through the existing
sync layer. **Nothing is sent automatically.**

AI options, daily usage, queued jobs and style checkpoints live in
`$XDG_STATE_HOME/pronto/ai.json`. The queue contains message IDs; style checkpoints
contain intermediate summaries and each account's last-processed date/UID, without
raw sampled bodies. Disabling auto-drafting clears pending reply
jobs and keeps completed normal drafts. Disabling style matching stops building
and using sheets, retaining them for later use.

## Tests

`tests/selftest/ai.sealwax-test` creates a private shell script that produces canned
text. It covers toggles, new-arrival boundaries, existing unread mail, filtering,
draft persistence/opening, the daily cap, style batching/resume, updating an edited
sheet with only new mail, no generation on an empty refresh, retries that keep
the job through an outage, and cancellation, and asserts that the outbox stays
empty. Style checkpoints are
checked for raw sample text, signatures, and quoted history. Unit tests cover
process timeout/cancellation, shell-safe arguments, sampling, filtering, prompt
construction and its untrusted-mail boundary, the `NO_REPLY` answer, paragraph
breaks, the retry back-off, the unread cap, when the worker sleeps, which settings
interrupt work, and the store's atomic automatic-draft guard. The engine's fake Sent
server counts body fetches to check the sample and refresh limits separately.
