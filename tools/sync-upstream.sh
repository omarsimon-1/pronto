#!/usr/bin/env bash
# Usage: tools/sync-upstream.sh [--check] [--rebase] [--branch <name>] [--push]
#
# Brings upstream Sealwax (PixDevsApps/sealwax) into this fork on a sync branch of its own,
# never onto main. Steps: fetch upstream's main and the fork's main, say how far apart they
# are, and, when upstream has commits the fork lacks, make <branch> (default
# upstream-sync/<YYYY-MM-DD>) from the fork's main and merge upstream into it (--rebase:
# replay the fork's own commits onto upstream instead). Main is then updated only through a
# pull request from that branch. See docs/UPSTREAM-SYNC.md.
#
#   --check          Fetch and report only; change no branch (exit 0 whether behind or not).
#   --rebase         Rebase the sync branch onto upstream instead of merging upstream into it.
#   --branch <name>  The sync branch's name; it must not exist yet and must not be main.
#   --push           Push the sync branch to origin (git push -u) when the merge or rebase is
#                    clean. Main is never pushed.
#
# Environment: PRONTO_UPSTREAM_URL (default https://github.com/PixDevsApps/sealwax.git),
# PRONTO_UPSTREAM_BRANCH (default main), PRONTO_BASE_BRANCH (the fork's main, default
# main). The remote "upstream" is added when missing, and must point at that URL when present.
#
# Exit codes: 0 done (or already up to date), 1 conflicts left for you to resolve (the
# instructions are printed; nothing is pushed), 2 usage or a refused precondition.
#
# Only git runs here: nothing is built, so no toolchain or library (system Qt included) is
# touched. Build and test the sync branch with tools/ci.sh before merging its pull request.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

upstream_url=${PRONTO_UPSTREAM_URL:-https://github.com/PixDevsApps/sealwax.git}
upstream_branch=${PRONTO_UPSTREAM_BRANCH:-main}
base=${PRONTO_BASE_BRANCH:-main}
mode=merge
check=false
push=false
branch=upstream-sync/$(date -u +%Y-%m-%d)

usage() {
    echo "usage: tools/sync-upstream.sh [--check] [--rebase] [--branch <name>] [--push]" >&2
    exit 2
}
die() {
    echo "sync-upstream: $*" >&2
    exit 2
}

while (($#)); do
    case $1 in
        --check) check=true ;;
        --rebase) mode=rebase ;;
        --push) push=true ;;
        --branch)
            (($# >= 2)) || usage
            branch=$2
            shift
            ;;
        -h | --help)
            sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        *) usage ;;
    esac
    shift
done

[[ $branch != "$base" && $branch != main && $branch != master ]] ||
    die "the sync branch can't be '$branch': upstream never goes straight onto main"
git check-ref-format --branch "$branch" >/dev/null 2>&1 || die "'$branch' isn't a valid branch name"

# The remote: add it, or check that an existing one is the upstream we mean.
if current_url=$(git remote get-url upstream 2>/dev/null); then
    [[ ${current_url%.git} == "${upstream_url%.git}" ]] ||
        die "remote 'upstream' is $current_url, not $upstream_url; fix it with: git remote set-url upstream $upstream_url"
else
    git remote add upstream "$upstream_url"
fi

# A full history is needed to count and merge; a shallow clone is deepened first.
if [[ $(git rev-parse --is-shallow-repository) == true ]]; then
    git fetch --quiet --unshallow origin
fi
git fetch --quiet --no-tags origin "+refs/heads/$base:refs/remotes/origin/$base"
git fetch --quiet --tags upstream "+refs/heads/$upstream_branch:refs/remotes/upstream/$upstream_branch"

fork_ref=origin/$base
upstream_ref=upstream/$upstream_branch
read -r ahead behind < <(git rev-list --left-right --count "$fork_ref...$upstream_ref")
describe() { git describe --tags --always "$1" 2>/dev/null || git rev-parse --short "$1"; }

echo "Upstream: $upstream_url ($upstream_ref at $(git rev-parse --short "$upstream_ref"), $(describe "$upstream_ref"))"
echo "Fork:     $fork_ref at $(git rev-parse --short "$fork_ref") ($(describe "$fork_ref"))"
echo "The fork is $behind commit(s) behind and $ahead commit(s) ahead of $upstream_ref."
if ((behind > 0)); then
    echo
    echo "Upstream commits the fork lacks:"
    git log --oneline --no-decorate "$fork_ref..$upstream_ref" | head -50
    ((behind <= 50)) || echo "… and $((behind - 50)) more"
fi

if $check; then
    exit 0
fi
if ((behind == 0)); then
    echo "Nothing to sync: $fork_ref already contains $upstream_ref."
    exit 0
fi

# Never start with uncommitted work: a conflict would mix it with upstream's changes.
[[ -z $(git status --porcelain --untracked-files=no) ]] ||
    die "the working tree has uncommitted changes; commit or stash them first"
if git rev-parse --verify --quiet "refs/heads/$branch" >/dev/null; then
    die "branch '$branch' already exists; delete it (git branch -D $branch) or pass --branch <name>"
fi

start=$(git symbolic-ref --quiet --short HEAD || git rev-parse --short HEAD)
git switch --quiet --no-track -c "$branch" "$fork_ref"
echo
if [[ $mode == merge ]]; then verb="merging $upstream_ref into it"; else verb="rebasing it onto $upstream_ref"; fi
echo "On $branch (from $fork_ref); $verb …"

conflicts() {
    echo
    echo "CONFLICTS: the $mode stopped on $branch. Main and origin are untouched."
    echo "Files in conflict:"
    git diff --name-only --diff-filter=U | sed 's/^/  /'
    echo
    echo "To finish:"
    echo "  1. Edit each file above, keeping upstream's change and the fork's where both matter."
    echo "  2. git add <file> for each one."
    if [[ $mode == merge ]]; then
        echo "  3. git merge --continue"
    else
        echo "  3. git rebase --continue (repeat 1-3 for each commit that stops)"
    fi
    echo "  4. bash tools/ci.sh, then git push -u origin $branch and open a pull request into $base."
    echo
    echo "To give up instead:"
    echo "  git $mode --abort && git switch $start && git branch -D $branch"
    exit 1
}

if [[ $mode == merge ]]; then
    git merge --no-ff --no-edit -m "Merge $upstream_ref ($(describe "$upstream_ref")) into $branch" \
        "$upstream_ref" || conflicts
else
    git rebase "$upstream_ref" || conflicts
fi

echo
echo "Clean: $branch now contains $upstream_ref."
if $push; then
    git push -u origin "$branch"
    echo "Pushed $branch. Open a pull request from it into $base."
else
    echo "Next: bash tools/ci.sh, then git push -u origin $branch and open a pull request into $base."
fi
if [[ $mode == rebase ]]; then
    echo "(A rebased branch rewrites the fork's commits: merge its pull request with a merge commit or rebase, not by force-pushing main.)"
fi
