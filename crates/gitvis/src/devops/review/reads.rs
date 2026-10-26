//! Reads of the Review page: GitHub fetches into the state's `Fetch`
//! slots (one in-flight guard each), the local read for the forms, and
//! what selecting a pull request, an issue or a tab fetches.

use super::*;

pub(super) fn err(e: gitvis_core::Error) -> String {
    e.to_string()
}

const NO_REMOTE: &str = "This repository has no GitHub remote.";

/// Start a GitHub read into `slot` unless one is running.
pub(super) fn gh_fetch<T>(
    cx: &mut Ctx,
    slot: &mut Fetch<T>,
    label: String,
    f: impl FnOnce(&Gh, &mut TaskLog) -> Msg + Send + 'static,
) {
    if !slot.begin() {
        return;
    }
    if !cx.gh_read(label, f) {
        slot.loading = false;
        slot.error = Some(NO_REMOTE.to_owned());
    }
}

pub(super) fn fetch_meta(cx: &mut Ctx, st: &mut State) {
    gh_fetch(
        cx,
        &mut st.meta,
        "Repository settings".to_owned(),
        |gh, log| {
            Msg::Meta(Box::new(
                fetch::fetch_repo_meta(gh, &mut log.calls).map_err(err),
            ))
        },
    );
}

pub(super) fn fetch_list(cx: &mut Ctx, st: &mut State, kind: ListKind, append: bool) {
    let after = if append {
        let Some(c) = st
            .list(kind)
            .data
            .as_ref()
            .and_then(|l| l.end_cursor.clone())
        else {
            return;
        };
        if st.more_loading {
            return;
        }
        st.more_loading = true;
        Some(c)
    } else {
        if !st.list_mut(kind).begin() {
            return;
        }
        None
    };
    let label = match (kind, append) {
        (ListKind::Open, false) => "Open pull requests",
        (ListKind::Open, true) => "More open pull requests",
        (ListKind::Closed, false) => "Closed pull requests",
        (ListKind::Closed, true) => "More closed pull requests",
    };
    let started = cx.gh_read(label, move |gh, log| Msg::List {
        kind,
        append,
        result: Box::new(
            fetch::fetch_pr_list(gh, kind, after.as_deref(), &mut log.calls).map_err(err),
        ),
    });
    if !started {
        st.more_loading = false;
        let l = st.list_mut(kind);
        l.loading = false;
        l.error = Some(NO_REMOTE.to_owned());
    }
}

pub(super) fn fetch_detail(cx: &mut Ctx, st: &mut State, n: u64) {
    let v = st.prs.entry(n).or_default();
    gh_fetch(
        cx,
        &mut v.detail,
        format!("Pull request #{n}"),
        move |gh, log| {
            Msg::Detail(
                n,
                Box::new(fetch::fetch_pr_detail(gh, n, &mut log.calls).map_err(err)),
            )
        },
    );
}

pub(super) fn fetch_files(cx: &mut Ctx, st: &mut State, n: u64) {
    let v = st.prs.entry(n).or_default();
    gh_fetch(
        cx,
        &mut v.files,
        format!("Files of pull request #{n}"),
        move |gh, log| Msg::Files(n, fetch::fetch_pr_files(gh, n, &mut log.calls).map_err(err)),
    );
}

pub(super) fn fetch_commits(cx: &mut Ctx, st: &mut State, n: u64) {
    let v = st.prs.entry(n).or_default();
    gh_fetch(
        cx,
        &mut v.commits,
        format!("Commits of pull request #{n}"),
        move |gh, log| {
            Msg::Commits(
                n,
                fetch::fetch_pr_commits(gh, n, &mut log.calls).map_err(err),
            )
        },
    );
}

/// Suggestions are fetched once the owner's kind is known (teams exist
/// only in organisations); `fill_base` calls this again when it arrives.
pub(super) fn fetch_suggestions(cx: &mut Ctx, st: &mut State) {
    let Some(org) = st.meta().owner_is_org else {
        return;
    };
    gh_fetch(
        cx,
        &mut st.suggestions,
        "People, labels and teams".to_owned(),
        move |gh, log| {
            Msg::Suggestions(Box::new(
                fetch::fetch_suggestions(gh, org, &mut log.calls).map_err(err),
            ))
        },
    );
}

pub(super) fn fetch_issues(cx: &mut Ctx, st: &mut State) {
    let key = st.issues.key();
    let login = st.viewer().map(str::to_owned);
    let label = format!(
        "Issues: {}{}",
        key.filter.label().to_lowercase(),
        key.label
            .as_deref()
            .map(|l| format!(", label {l}"))
            .unwrap_or_default()
    );
    let slot = st.issues.lists.entry(key.clone()).or_default();
    gh_fetch(cx, slot, label, move |gh, log| {
        let me = if key.filter.needs_login() && login.is_none() {
            gh.login(&mut log.calls).ok()
        } else {
            login
        };
        let pick = |f: IssueFilter| (key.filter == f).then(|| me.clone()).flatten();
        let q = fetch::IssueQuery {
            open: key.filter != IssueFilter::Closed,
            assignee: pick(IssueFilter::Assigned),
            creator: pick(IssueFilter::Created),
            mentioned: pick(IssueFilter::Mentioned),
            labels: key.label.iter().cloned().collect(),
        };
        let result = if key.filter.needs_login() && me.is_none() {
            Err("Could not tell who you are on GitHub (gh api user failed).".to_owned())
        } else {
            fetch::fetch_issues(gh, &q, 2, &mut log.calls).map_err(err)
        };
        Msg::Issues {
            key,
            login: me,
            result,
        }
    });
}

pub(super) fn fetch_issue(cx: &mut Ctx, st: &mut State, n: u64) {
    let slot = st.issues.details.entry(n).or_default();
    gh_fetch(cx, slot, format!("Issue #{n}"), move |gh, log| {
        Msg::Issue(
            n,
            Box::new(fetch::fetch_issue(gh, n, &mut log.calls).map_err(err)),
        )
    });
}

/// Templates (and, for the New pull request form, the commits between base
/// and head) from the working tree.
pub(super) fn read_local(cx: &mut Ctx, base_candidates: Vec<String>, head: Option<String>) {
    cx.read("Templates and commits", move |dir, log| {
        let templates = local::pr_templates(dir);
        let issue_templates = local::issue_templates(dir);
        let base_ref = local::first_resolving(dir, &base_candidates).unwrap_or_default();
        let commits = match (&head, base_ref.is_empty()) {
            (Some(h), false) => {
                log.command(local::commits_between_command(&base_ref, h, 200));
                Some(local::commits_between(dir, &base_ref, h, 200).map_err(err))
            }
            (Some(_), true) => Some(Err(format!(
                "None of {} exists locally; fetch first.",
                base_candidates.join(", ")
            ))),
            (None, _) => None,
        };
        Msg::Local(Box::new(LocalInfo {
            templates,
            issue_templates,
            commits,
            base_ref,
        }))
    });
}

/// Fetch what the current tab of the selected pull request needs.
pub(super) fn tab_shown(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.review;
    let Some(n) = st.selected else {
        return;
    };
    match st.tab {
        PrTab::Conversation => {}
        PrTab::Files => {
            if st.prs.get(&n).is_none_or(|v| v.files.wanted()) {
                fetch_files(cx, st, n);
            }
        }
        PrTab::Commits => {
            if st.prs.get(&n).is_none_or(|v| v.commits.wanted()) {
                fetch_commits(cx, st, n);
            }
        }
        PrTab::Checks => {
            if let Some(oid) = st.find(n).and_then(|p| Oid::from_str(&p.head_oid).ok())
                && !repo.checks.contains_key(&oid)
            {
                cx.action(Action::RefreshChecks(oid));
            }
        }
    }
}

pub(super) fn select_pr(cx: &mut Ctx, repo: &mut RepoState, n: u64) {
    let st = &mut repo.devops.review;
    st.mode = Mode::Prs;
    st.form_open = false;
    st.selected = Some(n);
    if st.prs.get(&n).is_none_or(|v| v.detail.wanted()) {
        fetch_detail(cx, st, n);
    }
    tab_shown(cx, repo);
}

pub(super) fn select_issue(cx: &mut Ctx, st: &mut State, n: u64) {
    st.mode = Mode::Issues;
    st.issues.form_open = false;
    st.issues.selected = Some(n);
    if st.issues.details.get(&n).is_none_or(|f| f.wanted()) {
        fetch_issue(cx, st, n);
    }
}

/// Read the commits between the New pull request form's base and head
/// (when both are known), for the title, the body and the summary.
pub(super) fn refresh_commits(cx: &mut Ctx, repo: &mut RepoState) {
    let Some(f) = repo.devops.review.new_pr.as_ref() else {
        return;
    };
    if f.base.is_empty() || f.head.is_empty() {
        return;
    }
    let (base, head) = (f.base.clone(), f.head.clone());
    let candidates = base_candidates(repo, &base);
    if let Some(f) = repo.devops.review.new_pr.as_mut() {
        f.commits = None;
    }
    read_local(cx, candidates, Some(head));
}

/// Fill the New pull request form's base once the default branch is known,
/// and fetch the suggestions a form waits for.
pub(super) fn fill_base(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.review;
    let wants = st.new_pr.is_some() || st.issues.form.is_some() || st.issues.edit.is_some();
    if wants && st.suggestions.wanted() {
        fetch_suggestions(cx, st);
    }
    let default = repo.devops.review.meta().default_branch;
    let Some(f) = repo.devops.review.new_pr.as_mut() else {
        return;
    };
    if f.base.is_empty() && !default.is_empty() {
        f.base = default;
        refresh_commits(cx, repo);
    }
}

pub(super) fn refresh(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.review;
    st.meta.error = None;
    fetch_meta(cx, st);
    match st.mode {
        Mode::Prs => {
            let kind = st.filter.kind();
            fetch_list(cx, st, kind, false);
            if let Some(n) = st.selected {
                apply(cx, repo, Act::RefreshPr(n));
            }
        }
        Mode::Issues => {
            fetch_issues(cx, st);
            if let Some(n) = st.issues.selected {
                fetch_issue(cx, st, n);
            }
        }
    }
}
