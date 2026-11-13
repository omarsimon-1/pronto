# Pronto and its Sealwax heritage

Pronto is a fork of [Sealwax (PixDevsApps/sealwax)](https://github.com/PixDevsApps/sealwax) by Fredrick Thorsen and the Sealwax contributors, taken at Sealwax 0.5.1. Sealwax's commit history is kept in this repository, and its MIT license and copyright notice are preserved in [LICENSE](LICENSE). Upstream changes are merged through a sync branch ([docs/UPSTREAM-SYNC.md](docs/UPSTREAM-SYNC.md)).

Pronto is a separate application from Sealwax: its own name, application ID, package and folders. The two can be installed side by side and share nothing.

| Identifier | Pronto |
|---|---|
| Application, D-Bus and Wayland ID | `io.github.omarsimon1.Pronto` |
| Debug application ID | `io.github.omarsimon1.Pronto.Devel` |
| Binary and Arch package | `pronto` |
| Desktop file and icon name | `io.github.omarsimon1.Pronto` |
| AppStream metainfo | `io.github.omarsimon1.Pronto.metainfo.xml` |
| Configuration | `$XDG_CONFIG_HOME/pronto` (normally `~/.config/pronto`) |
| Mail data | `$XDG_DATA_HOME/pronto` (normally `~/.local/share/pronto`) |
| Session state | `$XDG_STATE_HOME/pronto` (normally `~/.local/state/pronto`) |
| Cache | `$XDG_CACHE_HOME/pronto` (normally `~/.cache/pronto`) |
| Runtime files | `$XDG_RUNTIME_DIR/io.github.omarsimon1.Pronto` |
| Secret Service `application` attribute | `io.github.omarsimon1.Pronto` |

Public environment variables use `PRONTO_`. Internal Rust library names (`sealwax_*`), private editor hooks and `.sealwax-test` filenames keep their inherited names.

## What Pronto adds to Sealwax

- A split inbox, reminders (Remind Me), a contact details pane and conversation threading changes.
- Keyboard and command-palette changes modelled on Superhuman's workflow; `docs/superhuman/` holds the design notes.
- Per-account signatures.
- Sign in with Google, with your own Google OAuth client.
- Optional agent-backed reply drafts and writing-style sheets, off unless you turn them on.

## Migration from Dispatchy

Before it was published, Pronto was developed under the name Dispatchy. Nobody who installs Pronto fresh is affected. On a machine that had Dispatchy, the installed app copies missing configuration, mail data, state and cache from the `dispatchy` folders on first launch, and copies missing account passwords and refresh tokens from the `io.github.omarsimon1.Dispatchy` keyring scope. Existing Pronto files and secrets are never replaced, and the old copies are left in place. Quit Dispatchy first: Pronto refuses to copy while it is running. The package declares `conflicts` and `replaces` for `dispatchy`.

## Building

`tools/build-package.sh` builds committed HEAD into an Arch package using `tools/package-source.py`, `packaging/PKGBUILD.in` and makepkg, for the host architecture (x86_64 or aarch64). It refuses options that invoke sudo, checks that the binary does not link Qt, and prints the install command. See [INSTALL.md](INSTALL.md) and [docs/BUILD.md](docs/BUILD.md).

There are no per-push CI workflows; checks run locally with `bash tools/ci.sh`. Use `tools/headless.sh` for GTK self-tests so they do not open on the desktop.
