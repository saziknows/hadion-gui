//! Local daily workflow: task discovery and running (package.json scripts
//! with the right package manager, Makefile, Cargo, justfile, Taskfile,
//! composer, deno, Go, .NET, Gradle / Maven wrappers), interactive rebase
//! (plan editing: pick / reword / edit / squash / fixup / drop / reorder,
//! executed through `GIT_SEQUENCE_EDITOR`), bisect (manual good / bad /
//! skip and `git bisect run` with a test command), worktrees (list, add,
//! remove, prune, lock / unlock) and file history (`git log --follow` with
//! renames and per-commit changes).
