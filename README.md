<p align="center">
  <img src="docs/brand/pronto-icon-1200.png" alt="Pronto" width="160">
</p>

# Pronto

Mail, ready to go. A fast, keyboard-first email client for [Omarchy](https://omarchy.org): native GTK 4 and libadwaita, written in Rust, following your live Omarchy theme and font, and careful with every message: no script in one ever runs, and nothing from the internet loads until you allow it.

Pronto's workflow is inspired by Superhuman: a split inbox, reminders, one-key triage, a command palette and send-and-archive. It is an independent project and is not affiliated with Superhuman.

## Credits

Pronto is a fork of **[Sealwax](https://github.com/PixDevsApps/sealwax)** by Fredrick Thorsen and the Sealwax contributors, starting from Sealwax 0.5.1. The mail engine, the safe HTML reader, the composer, the Omarchy theming and the test suite are their work, and this repository keeps their commit history. Sealwax is MIT licensed; Pronto keeps that license and its copyright notice ([LICENSE](LICENSE)). If you want the original, minimal client, use Sealwax.

![Upstream Sealwax preview in the matte-black theme, showing fictional mail](tools/progress/public/m6-reply-conversation-matte-black-square-2026-10-06.png)

## Install

There is no prebuilt download yet. Build a package from source on Omarchy or Arch Linux:

```sh
git clone https://github.com/omarsimon-1/pronto.git
cd pronto
tools/build-package.sh            # add --nodeps when Rust comes from rustup
```

The script builds for your machine's architecture (x86_64 or aarch64) and prints the package path and the `sudo pacman -U` command to install it. It never runs sudo itself. Then start `pronto`, or choose **Pronto** in the launcher. [INSTALL.md](INSTALL.md) covers dependencies, updating and removing the app.

**Check for Updates** asks this repository's GitHub releases, so it reports no release until one is published.

## Status

**Based on Sealwax 0.5.1.** Pronto adds a split inbox, reminders, contact details, conversation threading and layout changes, per-account signatures, optional agent-backed reply drafts, and Sign in with Google using your own OAuth client. Historical release and test records in `docs/` describe the upstream Sealwax work. [FORK.md](FORK.md) lists what differs from Sealwax.

## Features

- **Accounts without server settings:** type your address, and Pronto finds the servers (its provider list, your domain's autoconfig, Mozilla's ISPDB, DNS). **Sign in with Microsoft** for Outlook.com and Microsoft 365; app passwords for iCloud, Fastmail, Yahoo and others, with a link to the provider's page; manual settings for the rest. Several accounts and All Inboxes, each account with a name and a muted colour of your choosing, folding away in the sidebar.
- **IMAP** with IDLE push and an offline copy of your mail, and **POP3** into local folders (mail left on the server by default).
- **Reading:** whole conversations; safe HTML (no message script ever runs; nothing remote loads until you allow it, per message or per sender); attachments that open safely, with a warning for programs and disguised files; link warnings; print, Show Original, Save as `.eml`; graceful display of encrypted and signed mail.
- **Triage:** archive, delete, junk, move, star, read and unread at once, on one message or many, with Undo, also offline; a right-click menu on messages.
- **Folders and rules:** new subfolders, rename and delete, shown as a tree you can fold; rules in Sieve, the standard mail-filter language, made in a Rules dialog or by hand in `rules/<account>.sieve`, run by Pronto on new mail (move, copy, mark read, star, junk, delete), with always-junk and never-junk senders and Create Rule from Message.
- **Writing:** a rich-text composer docked in the main window (minimize, expand, several at once), reply, reply all, forward and a quick reply under the conversation; identities and signatures; attachments with the server's size limit checked; drafts saved as you type and kept on the server; an outbox with Undo send that never sends a message twice; `mailto:` links, and becoming the default mail app when you choose.
- **Search** on this computer with `from:`, `to:`, `subject:`, `has:attachment`, `is:unread`, `before:`, `after:` and suggestions as you type.
- **New-mail notifications** that open the message, and **background mode**: closing the window keeps mail syncing; Start Pronto at Login.
- **Omarchy:** the live theme and font, light and dark; corners that follow Hyprland; no title bar on Hyprland; layouts for wide, half-screen and narrow tiled windows, with panes you can drag wider or narrower.
- **Keyboard first:** one-key shortcuts in the list and reader, a command palette (F1), and your own keys in `keys.toml`. Settings live in plain files: `config.toml`, `keys.toml`, `accounts.toml`.
- **Private:** passwords and tokens only in your keyring; no telemetry; logs without mail content; outgoing mail without your computer's name or a Bcc header.

**Gmail** works with an app password, as [this step-by-step guide](docs/GMAIL.md) shows, or with **Sign in with Google** once you make your own Google OAuth client ([how](docs/GOOGLE-OAUTH-SETUP.md)); Pronto ships no Google client. Not in 1.0: server-side search, rules uploaded to the server (ManageSieve), PGP and S/MIME, calendars and contacts beyond autocomplete ([release notes](docs/RELEASE_NOTES.md#known-limitations)).

## Email signatures

Open **Settings → Edit Account → Signature** and enter plain text on multiple lines. An empty field disables the signature. New messages, replies and forwards insert it below the writing area and above quoted text, with the standard `-- ` separator. The signature stays visible and editable in each composer.

## OAuth configuration

No OAuth client ID, client secret, account password or token belongs in this repository. Supply your own registration through `~/.config/pronto/config.toml` (or `$XDG_CONFIG_HOME/pronto/config.toml`):

```toml
[oauth.microsoft]
client_id = "YOUR_MICROSOFT_CLIENT_ID"

[oauth.google]
client_id = "YOUR_GOOGLE_CLIENT_ID"
client_secret = "YOUR_GOOGLE_CLIENT_SECRET"
```

Keep that file private (`chmod 600 ~/.config/pronto/config.toml`). Google credentials can also come from `PRONTO_GOOGLE_CLIENT_ID` and `PRONTO_GOOGLE_CLIENT_SECRET` in the launch environment. For a private build, those variables and `PRONTO_MICROSOFT_CLIENT_ID` can be supplied at build time; compiled credentials are extractable, so do not distribute a build containing your Google client secret. `tools/package-source.py` reads the Microsoft ID from the build environment or local settings, with `--no-client-id` to omit it. Generated build files stay under ignored `.local/`. Without client registration, use your provider's supported app-password flow. [Google setup](docs/GOOGLE-OAUTH-SETUP.md) and [Microsoft setup](docs/OAUTH-SETUP.md) explain registration.

## Requirements

Omarchy, or Arch Linux, on x86_64, with GTK 4.18+, libadwaita 1.9+, WebKitGTK 6.0 2.52+, SQLite 3.43+, Adwaita Sans (`adwaita-fonts`) and a Secret Service such as gnome-keyring; optionally `hunspell` and a hunspell dictionary for spell checking. Omarchy 4 is the desktop it is made for. Building needs Rust 1.92+.

## Documentation

- [User guide](docs/USER_GUIDE.md): setup (Microsoft sign-in, app passwords, POP3), reading, writing, search, notifications, every keyboard shortcut, theming, the settings files, privacy and troubleshooting
- [Install](INSTALL.md): installing, updating and uninstalling, in plain words; building from source
- [What's new](CHANGELOG.md): each release
- [Release notes for 1.0.0](docs/RELEASE_NOTES.md) (a draft until the sign-off)
- [Security](docs/SECURITY.md): the threat model, the rules and the known limitations
- [Integrations](docs/INTEGRATIONS.md): Omarchy, Hyprland key bindings and menu entries, providers
- [Microsoft sign-in setup](docs/OAUTH-SETUP.md): registering a client ID
- [Google sign-in setup](docs/GOOGLE-OAUTH-SETUP.md): making your own Google OAuth client for Gmail
- [Building](docs/BUILD.md) and [testing](docs/TESTING.md)
- [Decisions](docs/DECISIONS.md) and [design](docs/DESIGN.md)

## Thanks

Thanks to Fredrick Thorsen and the Sealwax contributors, on whose work Pronto is built, and to [@ocewers](https://github.com/ocewers) for detailed reports and ideas (issues #2 to #8).

License: MIT.
