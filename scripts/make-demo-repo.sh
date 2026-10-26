#!/usr/bin/env bash
# Builds a demo repository with branches, merges, tags, a remote, a stash and
# a dirty working tree, for exercising the gitvis UI.
set -euo pipefail

ROOT="$1"
rm -rf "$ROOT"
mkdir -p "$ROOT/work"
cd "$ROOT/work"

git init -q -b main
git config user.name "Ada Lovelace"
git config user.email ada@example.com
git config commit.gpgsign false
git config tag.gpgsign false
git config core.autocrlf false

T=$(( $(date +%s) - 45*86400 ))
tick() { T=$(( T + 3600 * ( 3 + RANDOM % 20 ) )); }

AUTHORS=("Ada Lovelace <ada@example.com>" "Grace Hopper <grace@example.com>" "Linus Benedict <linus@example.com>" "Margaret Hamilton <margaret@example.com>")

c() { # c <file> <message> [author-index]
  local file="$1" msg="$2" ai="${3:-0}"
  mkdir -p "$(dirname "$file")"
  echo "$msg ($RANDOM)" >> "$file"
  git add -A
  tick
  GIT_AUTHOR_DATE="$T +0000" GIT_COMMITTER_DATE="$T +0000" \
    git commit -q -m "$msg" --author="${AUTHORS[$ai]}"
}
m() { # m <branch> <message>
  tick
  GIT_AUTHOR_DATE="$T +0000" GIT_COMMITTER_DATE="$T +0000" git merge -q --no-ff "$1" -m "$2"
}

c README.md "Initial commit"
c src/main.rs "Add application skeleton" 1
c src/lib.rs "Introduce core library crate" 0
c Cargo.toml "Pin dependency versions" 2

git switch -q -c feature/auth
c src/auth/mod.rs "auth: add login form" 1
c src/auth/session.rs "auth: persist sessions in cookie store" 1
git switch -q main
c src/main.rs "Wire up logging" 3
c docs/ARCHITECTURE.md "Document module layout" 0
git switch -q feature/auth
c src/auth/oauth.rs "auth: support OAuth2 providers" 1
git switch -q main
m feature/auth "Merge branch 'feature/auth'"
git tag v0.1.0

git switch -q -c feature/graph
c src/graph/layout.rs "graph: lane allocation" 2
c src/graph/render.rs "graph: draw bezier connectors" 2
git switch -q main
c src/main.rs "Fix panic on empty repository" 0
c tests/smoke.rs "Add smoke tests" 3
git switch -q -c release/1.x
c CHANGELOG.md "Prepare 1.0 release notes" 0
tick; GIT_COMMITTER_DATE="$T +0000" git tag -a v1.0.0 -m "Release 1.0.0"
git switch -q main
c src/lib.rs "Refactor error type" 2
git switch -q feature/graph
c src/graph/render.rs "graph: colour lanes by index" 2
c src/graph/layout.rs "graph: reuse freed lanes" 2
git switch -q main
m feature/graph "Merge branch 'feature/graph'"
c README.md "Update screenshots" 3

git switch -q release/1.x
c src/lib.rs "Backport: fix error type regression" 0
tick; GIT_COMMITTER_DATE="$T +0000" git tag -a v1.0.1 -m "Release 1.0.1"
git switch -q main

git switch -q -c feature/ui/sidebar
c src/ui/sidebar.rs "sidebar: list branches as a tree" 1
c src/ui/sidebar.rs "sidebar: filter box" 1
git switch -q -c feature/ui/details main
c src/ui/details.rs "details: show commit metadata" 3
c src/ui/details.rs "details: file list with stats" 3
git switch -q main
c Cargo.toml "Bump egui" 0
m feature/ui/details "Merge branch 'feature/ui/details'"
c src/ui/details.rs "details: wrap long messages" 3
git switch -q feature/ui/sidebar
c src/ui/sidebar.rs "sidebar: drag and drop branches" 1

git switch -q main
c docs/ARCHITECTURE.md "Describe worker threads" 2
git tag v1.1.0

# Remote with everything so far.
git clone -q --bare "$ROOT/work" "$ROOT/origin.git"
git remote add origin "$ROOT/origin.git"
git fetch -q origin
git branch -q --set-upstream-to=origin/main main
git branch -q --set-upstream-to=origin/feature/ui/sidebar feature/ui/sidebar
git branch -q --set-upstream-to=origin/release/1.x release/1.x

# Diverge: two local commits on main, one remote-only commit on release/1.x.
c src/main.rs "Speed up startup" 0
c src/ui/toolbar.rs "toolbar: pull/push/fetch" 1
git push -q origin main:refs/heads/main 2>/dev/null || true
git reset -q --hard HEAD~1
git -C "$ROOT/origin.git" update-ref refs/heads/release/1.x refs/heads/main
git fetch -q origin

git switch -q -c hotfix/crash-on-close
c src/main.rs "Fix crash when closing last tab" 3
git switch -q main

# Stash and dirty working tree.
echo "scratch" > scratch.txt
git add scratch.txt
git stash push -q -m "WIP: scratch experiment"
echo "// TODO: polish" >> src/main.rs
echo "fn placeholder() {}" >> src/lib.rs
git add src/lib.rs
echo "notes" > NOTES.md
mkdir -p assets && printf '\x89PNG\r\n\x1a\n\0\0' > assets/logo.png

git log --oneline --graph --all | head -60
echo "repo at $ROOT/work"
