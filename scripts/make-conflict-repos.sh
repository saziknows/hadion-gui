#!/usr/bin/env bash
# Builds scratch repositories stopped on merge conflicts, for the conflict
# editor (History page: click a conflicted file in the WIP panel).
#
#   DIR/merge   a merge of `feature` into `main` (merge style) with:
#               src/lib.rs        three conflicting hunks
#               notes.md          a conflict next to Markdown "=======" lines
#               docs/guide.rst    conflict-marker-size=12 and "=======" content
#               windows.txt       CRLF line endings
#               eof.txt           a conflict at the end, no final newline
#               new_module.py     add/add (both sides created it)
#               assets/logo.png   binary on both sides
#               config/old.yml    deleted by them, modified by us
#               legacy.txt        deleted by us, modified by them
#   DIR/diff3   the same src/lib.rs conflict in diff3 style
#   DIR/zdiff3  the same in zdiff3 style
#   DIR/rebase  `topic` rebased onto `main`, stopped at step 2 of 3
#
# Usage: scripts/make-conflict-repos.sh DIR   (DIR is emptied first)
set -euo pipefail

ROOT="$1"
rm -rf "$ROOT"
mkdir -p "$ROOT"

init() { # init <dir>
  mkdir -p "$1"
  cd "$1"
  git init -q -b main
  git config user.name "Ada Lovelace"
  git config user.email ada@example.com
  git config commit.gpgsign false
  git config core.autocrlf false
  git config merge.conflictStyle merge
}

commit() { # commit <message> [author]
  git add -A
  git commit -q -m "$1" --author="${2:-Ada Lovelace <ada@example.com>}"
}

lib_base() {
  cat <<'EOF'
//! Order totals.

use std::collections::HashMap;

/// Sales tax applied to every order.
pub const TAX_RATE: f64 = 0.20;

/// The total of an order, tax included.
pub fn total(prices: &[f64]) -> f64 {
    let sum: f64 = prices.iter().sum();
    sum * (1.0 + TAX_RATE)
}

/// How many of each item.
pub fn count(items: &[&str]) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for item in items {
        *map.entry(item.to_string()).or_insert(0) += 1;
    }
    map
}

/// A short receipt line.
pub fn receipt(total: f64) -> String {
    format!("Total: {total:.2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals() {
        assert_eq!(total(&[10.0]), 12.0);
    }
}
EOF
}

# Three separate conflicts: the tax rate, the counting map, the receipt.
lib_ours() {
  lib_base | sed \
    -e 's/pub const TAX_RATE: f64 = 0.20;/pub const TAX_RATE: f64 = 0.21;/' \
    -e 's/    let mut map = HashMap::new();/    let mut map = HashMap::with_capacity(items.len());/' \
    -e 's/    format!("Total: {total:.2}")/    format!("Total due: {total:.2} EUR")/'
}
lib_theirs() {
  lib_base | sed \
    -e 's/pub const TAX_RATE: f64 = 0.20;/pub const TAX_RATE: f64 = 0.19; \/\/ reduced rate/' \
    -e 's/    let mut map = HashMap::new();/    let mut map: HashMap<String, usize> = HashMap::new();/' \
    -e 's/    format!("Total: {total:.2}")/    let rounded = (total * 100.0).round() \/ 100.0;\n    format!("Total: {rounded}")/'
}

three_hunks() { # three_hunks <dir> <style>
  init "$1"
  mkdir -p src
  lib_base > src/lib.rs
  commit "Order totals"
  git checkout -q -b feature
  lib_theirs > src/lib.rs
  commit "Reduced tax rate, typed map, rounded receipt" "Grace Hopper <grace@example.com>"
  git checkout -q main
  lib_ours > src/lib.rs
  commit "New tax rate, sized map, receipt in EUR"
  git -c merge.conflictStyle="$2" merge feature >/dev/null 2>&1 || true
}

# --- merge -------------------------------------------------------------
init "$ROOT/merge"
mkdir -p src docs assets config
lib_base > src/lib.rs
printf 'Release notes\n=======\n\nVersion 1.0\n-----------\n\n- First release.\n' > notes.md
printf '*.rst conflict-marker-size=12\n*.png binary\n' > .gitattributes
printf 'Guide\n=====\n\nInstall\n-------\n\nRun the installer.\n' > docs/guide.rst
printf 'line one\r\nline two\r\nline three\r\n' > windows.txt
printf 'first line\nlast line' > eof.txt
printf '\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDRbase' > assets/logo.png
printf 'retries: 3\ntimeout: 30\n' > config/old.yml
printf 'legacy mode\n' > legacy.txt
commit "Base project"

git checkout -q -b feature
lib_theirs > src/lib.rs
printf 'Release notes\n=======\n\nVersion 1.1\n-----------\n\n- Faster startup.\n' > notes.md
printf 'Guide\n=====\n\nSetup\n=======\n\nRun setup.sh.\n' > docs/guide.rst
printf 'line one\r\nline TWO from feature\r\nline three\r\n' > windows.txt
printf 'first line\nlast line from feature' > eof.txt
printf 'import os\n\ndef main():\n    print(os.getcwd())\n' > new_module.py
printf '\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDRtheirs-and-longer' > assets/logo.png
git rm -q config/old.yml
printf 'legacy mode, kept by feature\nwith a note\n' > legacy.txt
commit "Feature work" "Grace Hopper <grace@example.com>"

git checkout -q main
lib_ours > src/lib.rs
printf 'Release notes\n=======\n\nVersion 1.0.1\n-------------\n\n- Bug fixes.\n' > notes.md
printf 'Guide\n=====\n\nInstallation\n=======\n\nUse the package manager.\n' > docs/guide.rst
printf 'line one\r\nline 2 from main\r\nline three\r\n' > windows.txt
printf 'first line\nlast line from main' > eof.txt
printf 'import sys\n\ndef main():\n    print(sys.argv)\n' > new_module.py
printf '\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDRours' > assets/logo.png
printf 'retries: 5\ntimeout: 30\n' > config/old.yml
git rm -q legacy.txt
commit "Main work"
git merge feature >/dev/null 2>&1 || true

# --- diff3 / zdiff3 ----------------------------------------------------
three_hunks "$ROOT/diff3" diff3
three_hunks "$ROOT/zdiff3" zdiff3

# --- rebase ------------------------------------------------------------
init "$ROOT/rebase"
mkdir -p src
lib_base > src/lib.rs
commit "Order totals"
git checkout -q -b topic
printf '# Orders\n' > README.md
commit "Add a readme"
lib_theirs > src/lib.rs
commit "Reduced tax rate, typed map, rounded receipt"
printf 'MIT\n' > LICENSE
commit "Add a licence"
git checkout -q main
lib_ours > src/lib.rs
commit "New tax rate, sized map, receipt in EUR"
git checkout -q topic
git rebase main >/dev/null 2>&1 || true

cd "$ROOT"
for d in merge diff3 zdiff3 rebase; do
  printf '%-7s %s\n' "$d" "$(git -C "$d" status --porcelain=v2 | grep -c '^u ' || true) conflicted files"
done
