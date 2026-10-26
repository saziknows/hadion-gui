#!/usr/bin/env bash
# Build a throwaway repository with planted secrets for testing the
# Exposure page: a token committed and removed again (history only), a
# private key still tracked, an untracked key file, a staged secret, a
# Terraform state with a password, a remote URL with a password, and one
# commit that is not pushed. Nothing in it is a real credential.
#
#   scripts/make-leaky-repo.sh /tmp/leaky
#   GITVIS_SCRIPT="wait 3; security; scan-history 0; wait 5; shot out.png; quit" target/debug/gitvis.exe /tmp/leaky
set -euo pipefail
D=${1:?usage: make-leaky-repo.sh DIR}
rm -rf "$D" "$D-remote.git"
mkdir -p "$D"
cd "$D"
git init -q -b main
git config user.name "Dev"
git config user.email "dev@example.com"
git config commit.gpgsign false
git config core.autocrlf false

printf '# app\n' > README.md
git add -A && git commit -q -m "init"

# A token in .env, committed and then removed: still in history.
printf 'GITHUB_TOKEN=ghp_z1Yx2Wv3Ut4Sr5Qp6On7Ml8Kj9Ih0Hg1Fe2Dc3Ba4\nDATABASE_URL=postgres://app:Tr0ub4dor3xyz@db.internal:5432/app\nPORT=8080\n' > .env
git add -A && git commit -q -m "add env for deploy"
git rm -q .env && git commit -q -m "remove env (oops)"

mkdir -p deploy src config
printf -- '-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n-----END OPENSSH PRIVATE KEY-----\n' > deploy/id_rsa
printf 'import os\n\nSTRIPE_KEY = "sk_live_51H8sJ2KzQyXbW3n9LmP7Rt0V"\nSLACK = "xoxb-917203645081-2938475610293-Qr7TmZp2Lk9WvXc4Yb8Nd3Hs"\npassword = os.environ["PW"]  # fine\n' > src/settings.py
printf 'aws:\n  access_key_id: AKIAJ6ZQMPTX3V7UBNRQ\n  secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYzQ9vL2mNpQ"\n' > config/prod.yml
printf 'const key = "AIzaSyD-9tSrke72PouQMnMX-a7eZSW0jkFMBWY"; // gitvis:allow\nconst example = "AKIAIOSFODNN7EXAMPLE";\n' > src/client.js
git add -A && git commit -q -m "deploy tooling"

# Everything so far is on the remote; the next commit is local only.
git clone -q --bare . "$D-remote.git"
git remote add origin "$D-remote.git"
git fetch -q origin
git branch -q --set-upstream-to=origin/main main
printf 'terraform { }\n' > main.tf
printf '{"version": 4, "resources": [{"instances": [{"attributes": {"password": "Zq8!vP2mLw9xT#r4"}}]}]}\n' > terraform.tfstate
git add -A && git commit -q -m "infra (local only)"

# OSINT material: a second identity with a personal address and an alias,
# footprint identifiers, a deleted key file, a stash with a secret, a
# backup-named branch, an amended (rewritten) commit, junk, a git-pinned
# dependency and a CODEOWNERS file.
git config user.email "dev.personal@gmail.com"
printf 'db_host: 10.20.30.40\nbucket: s3://acme-prod-backups\nowner: maria.lopez@acmecorp.io\nrole: arn:aws:iam::734019283745:role/deploy\n' > config/infra.yml
printf '*.rs @acme/rust-team @dev\n' > CODEOWNERS
git add -A && git commit -q -m "Add infra config and owners

Co-authored-by: Bob <bob@acmecorp.io>
Fixes ACME-123"
git config user.email "dev@example.com"
printf 'not a real key\n' > deploy.key
git add deploy.key && git commit -q -m "add deploy key"
git rm -q deploy.key && git commit -q -m "drop deploy key (oops, leaked)"
printf 'GITHUB_TOKEN=ghp_z1Yx2Wv3Ut4Sr5Qp6On7Ml8Kj9Ih0Hg1Fe2Dc3Ba4\n' > stashed.txt
git add stashed.txt && git stash push -q -m "wip notes"
git branch backup/old-main
printf 'a\n' > a.txt && git add a.txt && git commit -q -m "will be amended"
git commit -q --amend -m "amended"
printf 'junk\n' > .DS_Store && git add -f .DS_Store
printf '{"dependencies":{"left-pad":"^1.0.0","thing":"git+https://github.com/x/thing.git"}}\n' > package.json
git add -A && git commit -q -m "deps and junk"

# Working tree: a staged secret, an untracked key, a harmless file, a leaky remote.
printf 'OPENAI_API_KEY=sk-proj-abc123T3BlbkFJdef456ghi789jkl012mno345\n' > .env.local
git add .env.local
printf 'not really a key\n' > server.key
printf 'notes\n' > notes.txt
git remote add mirror https://bob:Sup3rS3cret@example.com/mirror.git

git log --oneline
git status --short

# Insights material: checkout landmines (a Windows-reserved name and a case
# collision added through the index so the script also runs on Windows), a
# CRLF file, a script without the executable bit, a tracked-but-ignored file,
# tags with unreleased work after them, a feature branch that will conflict,
# a cherry-picked duplicate, a stale throwaway branch, debt markers, a leaky
# workflow, a licence that disagrees with package.json and a foreign header.
git config user.email "dev@example.com"
printf 'a\r\nb\r\n' > notes-crlf.txt
mkdir -p tools && printf '#!/bin/sh\necho hi\n' > tools/run.sh
printf 'build/\n' > .gitignore
mkdir -p build && printf 'artifact\n' > build/out.txt
git add -A && git add -f build/out.txt
git commit -q -m "chore: add tooling"
# Windows git refuses reserved names even through the index, so build the
# tree by hand: HEAD's tree plus `aux.txt` and `README.MD` (a case collision
# with README.md), commit it and move main there. The two files cannot be
# checked out on Windows, which is exactly what the Health tab should say.
blob=$(printf 'reserved\n' | git hash-object -w --stdin)
tree=$( { git ls-tree HEAD; printf '100644 blob %s\taux.txt\n100644 blob %s\tREADME.MD\n' "$blob" "$blob"; } | git mktree)
c=$(git commit-tree "$tree" -p HEAD -m "chore: add files with portability problems")
git update-ref refs/heads/main "$c"
git config core.protectNTFS false   # let the index hold the reserved name on Windows
git read-tree HEAD
git tag -a v1.0.0 -m "First release"
printf '// TODO(bob): fix #12 handle timeouts\n// FIXME this leaks the connection\nfunction x() {}\n' > src/todo.js
printf 'MIT License\n\nCopyright (c) 2019 Dev\n\nPermission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND.\n' > LICENSE
printf '{"name":"app","license":"ISC","dependencies":{"left-pad":"^1.0.0","thing":"git+https://github.com/x/thing.git"}}\n' > package.json
printf '// Copyright (c) 2018 Other Corp\n// SPDX-License-Identifier: GPL-3.0-only\nmodule.exports = 1;\n' > src/vendored.js
mkdir -p .github/workflows
cat > .github/workflows/ci.yml <<'YML'
name: CI
on:
  pull_request_target:
  push:
jobs:
  build:
    runs-on: self-hosted
    steps:
      - uses: actions/checkout@v2
        with:
          ref: ${{ github.event.pull_request.head.sha }}
      - uses: someone/setup-thing@v1
        with:
          token: ${{ secrets.NPM_TOKEN }}
      - run: echo "${{ github.event.pull_request.title }}"
      - run: curl -s https://example.com/install.sh | sh
YML
git add src/todo.js LICENSE package.json src/vendored.js .github
git commit -q -m "feat: add debt, licence and workflow material"
git commit -q --allow-empty -m "fix: a small fix"
git commit -q --allow-empty -m "WIP"
git switch -q -c feature/conflict
printf '# app\nfeature line\n' > README.md
git add README.md && git commit -q -m "feat: feature edits README"
printf 'feature only\n' > feature-only.txt && git add feature-only.txt && git commit -q -m "feat: feature-only file"
git switch -q main
printf '# app\nmain line\n' > README.md
git add README.md && git commit -q -m "docs: main edits README"
git switch -q -c tmp-old HEAD~3
git switch -q main
git cherry-pick feature/conflict >/dev/null   # the feature-only commit: a duplicate patch for `git cherry`
printf 'stash me\n' > stash.txt && git add stash.txt && git stash push -q -m "dropped later" -- stash.txt && git stash drop -q
