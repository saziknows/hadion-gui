//! The review loop on GitHub: pull requests (list with filters, detail,
//! conversation, files and patches, review threads with resolution state,
//! create with template / reviewers / labels / draft, merge with
//! merge / squash / rebase, auto-merge, ready for review, close and reopen,
//! update branch), reviews (pending comments on lines and ranges,
//! suggestions, replies, approve / request changes / comment, resolve and
//! unresolve threads) and issues (list, detail with comments, create,
//! comment, close and reopen, labels and assignees, a branch name for an
//! issue, commits that reference it).
//!
//! Requests are built by pure functions returning [`super::gh::Request`]
//! (or GraphQL documents) so they can be unit tested.
