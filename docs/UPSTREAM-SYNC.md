# Upstream sync

Synced: 2026-10-22

Pronto is based on **[Sealwax (PixDevsApps/sealwax)](https://github.com/PixDevsApps/sealwax)**. The app's update check targets this repository's releases, while the upstream sync tool fetches Sealwax's `main`.

Upstream reaches the fork only through a **sync branch** and a pull request into `main`. Nothing merges or rebases onto `main` directly.

## How far behind is the fork?

```bash
tools/sync-upstream.sh --check
```

It adds the `upstream` remote when missing, fetches, and prints the commits behind and ahead and the refs compared. Last check (2026-10-22):

| Ref | Commit |
| --- | --- |
| `upstream/main` (PixDevsApps/sealwax) | `503c01a` (`v0.5.1-1-g503c01a`, 2026-10-20) |
| `origin/main` | `503c01a` |

**Historical snapshot only:** this describes the 2026-10-22 check.

The script accepts `PRONTO_UPSTREAM_URL`, `PRONTO_UPSTREAM_BRANCH` and `PRONTO_BASE_BRANCH` overrides. Defaults remain upstream Sealwax and the fork’s main branch; syncing is separate from releasing.

## Syncing by hand

```bash
tools/sync-upstream.sh                     # merge upstream into upstream-sync/<today>
tools/sync-upstream.sh --rebase            # replay the fork's own commits onto upstream instead
tools/sync-upstream.sh --branch my-sync    # another branch name
tools/sync-upstream.sh --push              # also push the branch when it is clean
```

When upstream has commits the fork lacks, the script makes the sync branch from `origin/main`, merges `upstream/main` into it (or rebases), and stops there. It refuses to start with uncommitted changes, with a sync branch that already exists, or with `main` as the sync branch, and it only ever pushes the sync branch. Then:

1. `bash tools/ci.sh` on the sync branch (fmt, Clippy with `-D warnings`, tests, build).
2. `git push -u origin <branch>` (unless `--push` did it) and open a pull request into `main`.

**Merge or rebase:** merge is the default and the safer choice: the fork's commits keep their hashes, and the pull request merges cleanly. Use `--rebase` only while the fork's own commits are few; the rebased branch has new hashes for them, so its pull request must be merged with "Rebase and merge" (or by fast-forward), never with a merge commit, or the fork's commits appear twice.

Exit codes: 0 done or nothing to do, 1 conflicts, 2 a refused precondition or bad usage.

## Conflicts

On a conflict the script stops on the sync branch, lists the conflicted files and prints what to do; `main` and `origin` are untouched and nothing is pushed:

```text
CONFLICTS: the merge stopped on upstream-sync/2026-10-22. Main and origin are untouched.
Files in conflict:
  app/src/…
To finish:
  1. Edit each file above, keeping upstream's change and the fork's where both matter.
  2. git add <file> for each one.
  3. git merge --continue            (rebase: git rebase --continue, for each commit that stops)
  4. bash tools/ci.sh, then git push -u origin <branch> and open a pull request into main.
To give up instead:
  git merge --abort && git switch <where you were> && git branch -D <branch>
```

## The weekly workflow (opt-in, off by default)

`.github/workflows/upstream-sync.yml` runs the same script on GitHub. Its only triggers are a weekly `schedule` (Mondays 06:17 UTC) and `workflow_dispatch`; there is no `push` or `pull_request` trigger, and it builds nothing.

- **Off by default:** a scheduled run skips its job unless the repository variable `PRONTO_UPSTREAM_SYNC` is `true` (Settings → Secrets and variables → Actions → Variables). Delete the variable, or set it to anything else, to turn it off again.
- **By hand:** Actions → "Upstream sync (opt-in)" → Run workflow, whatever the variable says. `check_only` reports how far behind the fork is and changes nothing; `mode` picks merge or rebase.
- **What it does:** when the fork is behind, it pushes `upstream-sync/<date>` and opens a pull request into `main` titled "Pronto: Upstream sync (…)", with the script's report. When the branch already exists it does nothing. On conflicts it pushes nothing and fails; the run's summary has the report and the instructions, and the conflict is resolved locally as above.
- It commits as `github-actions[bot]` and uses only the workflow's `GITHUB_TOKEN` (`contents: write`, `pull-requests: write`).

## Building a sync branch

`tools/build-package.sh` builds for the host architecture (x86_64 or aarch64). Build and test a sync branch with `bash tools/ci.sh` before merging (docs/BUILD.md lists the Arch packages: GTK 4, libadwaita, WebKitGTK 6.0, SQLite). Pronto is GTK only: a sync must never bring in a build step that finds, links or installs Qt, including the system's Qt 6.12. Check an upstream change to `Cargo.toml`, `Cargo.lock`, a `build.rs` or `packaging/` for a `qt`/`cxx-qt`/`qmake` dependency before merging it.
