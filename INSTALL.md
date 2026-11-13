# Installing Pronto

Pronto is a GTK 4 email client for Omarchy and Arch Linux, based on [Sealwax](https://github.com/PixDevsApps/sealwax). Upstream Sealwax is a separate package and can stay installed beside it.

## Build a package

You need `git`, Python 3.11+, `base-devel`, `pkgconf`, Rust 1.92+ and Cargo. Runtime and build libraries include GTK 4.18+, libadwaita 1.9+, WebKitGTK 6.0 2.52+, SQLite 3.43+, GLib and Adwaita fonts. Install missing system dependencies yourself; the build script never invokes sudo.

From this repository, with the intended changes committed:

```sh
tools/build-package.sh
```

If Rust was installed with rustup rather than pacman, use `tools/build-package.sh --nodeps` after checking the other dependencies are installed. The script packages committed HEAD with `tools/package-source.py` and `packaging/PKGBUILD.in`, selects the host architecture, and runs `makepkg --cleanbuild --force --nocheck`. Run the relevant tests before packaging; [testing](docs/TESTING.md) describes the headless checks.

The package is written to `.local/package/pronto-<version>-1-<arch>.pkg.tar.zst`, and the script prints the exact install command:

```sh
sudo pacman -U .local/package/pronto-<version>-1-x86_64.pkg.tar.zst
```

To install on another machine of the same architecture, copy the package file there and run the same command. Restart the application after updating.

## Start and sign in

Choose **Pronto** in the app launcher, or run `pronto`.

Add an account from the main menu. Google and Microsoft browser sign-in require your own OAuth registration supplied through local settings or build configuration; see the [README](README.md#oauth-configuration). Passwords and sign-in tokens are stored in a Secret Service such as gnome-keyring. Without one, they last only until the app exits.

Edit an account in Settings to set its plain-text signature. Empty means no signature. New messages, replies and forwards show it below the writing area and above quoted text, with the standard `-- ` separator, and you can edit it for each message.

**Check for Updates** asks this repository's GitHub releases and reports no release until one is published. Until then, update by pulling and running the build script again.

## Installed files

| Path | Purpose |
|---|---|
| `/usr/bin/pronto` | Program |
| `/usr/share/applications/io.github.omarsimon1.Pronto.desktop` | Launcher and mailto handler |
| `/usr/share/dbus-1/services/io.github.omarsimon1.Pronto.service` | D-Bus activation |
| `/usr/share/icons/hicolor/*/apps/io.github.omarsimon1.Pronto.*` | Icons |
| `/usr/share/metainfo/io.github.omarsimon1.Pronto.metainfo.xml` | AppStream metadata |
| `/usr/share/licenses/pronto/LICENSE` | MIT license |
| `/usr/share/doc/pronto/` | User documentation |

## Remove the app

To remove saved passwords and the local mail copy, remove accounts in Pronto first. Mail on your provider's server is retained. Turn **Start Pronto at Login** off, quit with **Ctrl+Q**, then run:

```sh
sudo pacman -R pronto
```

Uninstalling keeps your local settings and mail for a future installation. Their locations are `~/.config/pronto`, `~/.local/share/pronto`, `~/.local/state/pronto` and `~/.cache/pronto` (or their XDG equivalents). Remove these only if you want to discard that local data.

## Develop without touching your mail

```sh
cargo build -p pronto
scratch=$(mktemp -d)
XDG_CONFIG_HOME="$scratch/config" XDG_DATA_HOME="$scratch/data" \
  XDG_STATE_HOME="$scratch/state" XDG_CACHE_HOME="$scratch/cache" \
  tools/headless.sh 19 target/debug/pronto --self-test tests/selftest/window.sealwax-test
```

Debug builds use `io.github.omarsimon1.Pronto.Devel`. Ordinary runs otherwise use the same XDG folder names as production, so use scratch directories for testing. The `.sealwax-test` extension and internal `sealwax_*` Rust library names remain for compatibility with the inherited code and test format.
