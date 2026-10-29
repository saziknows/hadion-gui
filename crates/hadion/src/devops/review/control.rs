//! The Review page's controller: what each `Act` does, the replies of
//! background work, and the script commands that need the state.

use super::*;

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    let gh = cx.gh.clone();
    match act {
        Act::OpenPr(n) => {
            let st = &mut repo.devops.review;
            if st.meta.wanted() {
                fetch_meta(cx, st);
            }
            select_pr(cx, repo, n);
        }
        Act::Refresh => refresh(cx, repo),
        Act::LoadMore => {
            let st = &mut repo.devops.review;
            let kind = st.filter.kind();
            fetch_list(cx, st, kind, true);
        }
        Act::SetMode(m) => {
            repo.devops.review.mode = m;
            on_page_shown(cx, repo);
        }
        Act::SetFilter(f) => {
            let st = &mut repo.devops.review;
            st.filter = f;
            let kind = f.kind();
            if st.list(kind).wanted() {
                fetch_list(cx, st, kind, false);
            }
        }
        Act::SetTab(t) => {
            repo.devops.review.tab = t;
            tab_shown(cx, repo);
        }
        Act::OpenListed(i) => {
            let st = &mut repo.devops.review;
            match st.visible().get(i).map(|p| p.number) {
                Some(n) => select_pr(cx, repo, n),
                None => st.pending_listed = Some(i),
            }
        }
        Act::RefreshPr(n) => {
            let st = &mut repo.devops.review;
            fetch_detail(cx, st, n);
            let v = st.prs.entry(n).or_default();
            let (files, commits) = (v.files.data.is_some(), v.commits.data.is_some());
            if files {
                fetch_files(cx, st, n);
            }
            if commits {
                fetch_commits(cx, st, n);
            }
            if st.tab == PrTab::Checks
                && let Some(oid) = st.find(n).and_then(|p| Oid::from_str(&p.head_oid).ok())
            {
                cx.action(Action::RefreshChecks(oid));
            }
        }
        Act::SelectFileIndex(i) => {
            let st = &mut repo.devops.review;
            st.tab = PrTab::Files;
            let Some(n) = st.selected else {
                return;
            };
            let path = st
                .prs
                .get(&n)
                .and_then(|v| v.files.data.as_ref())
                .and_then(|f| f.get(i))
                .map(|f| f.path.clone());
            match path {
                Some(p) => st.prs.entry(n).or_default().file = Some(p),
                None => {
                    st.pending_file = Some(i);
                    tab_shown(cx, repo);
                }
            }
        }
        Act::ToggleViewed(key) => repo.devops.review.toggle_viewed(key),
        Act::AddPending(n) => {
            let st = &mut repo.devops.review;
            match composed(st, n) {
                Ok(c) => {
                    let v = st.prs.entry(n).or_default();
                    v.pending.push(c);
                    v.composer.clear();
                    v.composer_error.clear();
                    v.sel = None;
                }
                Err(e) => st.prs.entry(n).or_default().composer_error = e,
            }
        }
        Act::RemovePending(n, i) => {
            if let Some(v) = repo.devops.review.prs.get_mut(&n)
                && i < v.pending.len()
            {
                v.pending.remove(i);
            }
        }
        Act::Suggest(n) => suggest(&mut repo.devops.review, n),
        Act::SingleComment(n) => {
            let Some(gh) = gh else { return };
            let st = &mut repo.devops.review;
            let c = match composed(st, n) {
                Ok(c) => c,
                Err(e) => {
                    st.prs.entry(n).or_default().composer_error = e;
                    return;
                }
            };
            let Some(d) = loaded_detail(cx, st, n) else {
                return;
            };
            let change = Change {
                label: format!("Comment on #{n}"),
                success: format!("Commented on {} {}", c.path, c.range_label()),
                steps: vec![Step::Rest(rest::single_comment(&gh, n, &d.pr.head_oid, &c))],
                after: After::LineComment(n),
            };
            confirm_change(
                cx,
                format!("Comment on {} {}?", c.path, c.range_label()),
                "A single comment is published right away, outside a review.",
                "Comment",
                false,
                change,
            );
        }
        Act::SubmitReview(n) => {
            let Some(gh) = gh else { return };
            let st = &mut repo.devops.review;
            let Some(d) = loaded_detail(cx, st, n) else {
                return;
            };
            let v = st.prs.entry(n).or_default();
            if let Some(problem) =
                rest::review_problem(v.review_event, &v.review_body, v.pending.len())
            {
                cx.toast(ToastKind::Info, problem);
                v.review_open = true;
                return;
            }
            let event = v.review_event;
            let change = Change {
                label: format!("Review #{n}"),
                success: format!(
                    "Submitted a review on #{n}: {} with {}",
                    event.label().to_lowercase(),
                    format::plural(v.pending.len(), "comment", "comments")
                ),
                steps: vec![Step::Rest(rest::submit_review(
                    &gh,
                    n,
                    &d.pr.head_oid,
                    &v.review_body,
                    event,
                    &v.pending,
                ))],
                after: After::Review(n),
            };
            let body = format!(
                "Publishes your review with {} on {}.",
                format::plural(v.pending.len(), "line comment", "line comments"),
                short(&d.pr.head_oid)
            );
            confirm_change(
                cx,
                format!("{} on #{n}?", event.label()),
                body,
                event.label(),
                event == ReviewEvent::RequestChanges,
                change,
            );
        }
        Act::Reply { pr, thread } => {
            let Some(gh) = gh else { return };
            let st = &repo.devops.review;
            let Some(d) = loaded_detail(cx, st, pr) else {
                return;
            };
            let Some(t) = d.threads.iter().find(|t| t.id == thread) else {
                return;
            };
            let Some(to) = t.reply_to() else {
                cx.toast(ToastKind::Error, "This thread has no comment to reply to.");
                return;
            };
            let body = st
                .prs
                .get(&pr)
                .and_then(|v| v.replies.get(&thread))
                .cloned()
                .unwrap_or_default();
            if body.trim().is_empty() {
                return;
            }
            let change = Change {
                label: format!("Reply on #{pr}"),
                success: format!("Replied on {} {}", t.path, t.line_label()),
                steps: vec![Step::Rest(rest::reply_to_comment(&gh, pr, to, &body))],
                after: After::Reply(pr, thread.clone()),
            };
            confirm_change(
                cx,
                format!("Reply on {}?", t.path),
                format!("Published right away in the thread on {}.", t.line_label()),
                "Reply",
                false,
                change,
            );
        }
        Act::Resolve {
            pr,
            thread,
            resolve,
        } => {
            let st = &repo.devops.review;
            let Some(d) = loaded_detail(cx, st, pr) else {
                return;
            };
            let place = d
                .threads
                .iter()
                .find(|t| t.id == thread)
                .map(|t| format!("{} {}", t.path, t.line_label()))
                .unwrap_or_default();
            let m = if resolve {
                graphql::resolve_thread(&thread)
            } else {
                graphql::unresolve_thread(&thread)
            };
            let verb = if resolve { "Resolve" } else { "Unresolve" };
            confirm_change(
                cx,
                format!("{verb} the thread on {place}?"),
                "",
                verb,
                false,
                Change {
                    label: format!("{verb} a thread on #{pr}"),
                    success: format!(
                        "{} the thread on {place}",
                        if resolve { "Resolved" } else { "Reopened" }
                    ),
                    steps: vec![Step::Gql(m)],
                    after: After::Pr(pr),
                },
            );
        }
        Act::PostComment(n) => {
            let Some(gh) = gh else { return };
            let body = repo
                .devops
                .review
                .prs
                .get(&n)
                .map(|v| v.comment.clone())
                .unwrap_or_default();
            if body.trim().is_empty() {
                return;
            }
            confirm_change(
                cx,
                format!("Comment on #{n}?"),
                "Published right away in the conversation.",
                "Comment",
                false,
                Change {
                    label: format!("Comment on #{n}"),
                    success: format!("Commented on #{n}"),
                    steps: vec![Step::Rest(rest::issue_comment(&gh, n, &body))],
                    after: After::Comment(n),
                },
            );
        }
        Act::Merge(n) => {
            let Some(gh) = gh else { return };
            let st = &repo.devops.review;
            let Some(change) = merge_change(cx, st, &gh, n, false) else {
                return;
            };
            let pr = st.find(n).cloned().unwrap_or_default();
            let deletes = change.steps.len() > 1;
            confirm_change(
                cx,
                format!("Merge #{n} into {}?", pr.base),
                format!(
                    "GitHub refuses if {} moved past {} since it was loaded.{}",
                    pr.head,
                    short(&pr.head_oid),
                    if deletes {
                        format!(" Then {} is deleted on GitHub.", pr.head)
                    } else {
                        String::new()
                    }
                ),
                "Merge",
                false,
                change,
            );
        }
        Act::AutoMerge { pr: n, enable } => {
            let Some(gh) = gh else { return };
            let st = &repo.devops.review;
            if enable {
                let Some(change) = merge_change(cx, st, &gh, n, true) else {
                    return;
                };
                confirm_change(
                    cx,
                    format!("Enable auto-merge on #{n}?"),
                    "GitHub merges it as soon as the required reviews and checks pass.",
                    "Enable auto-merge",
                    false,
                    change,
                );
            } else {
                let Some(d) = loaded_detail(cx, st, n) else {
                    return;
                };
                confirm_change(
                    cx,
                    format!("Disable auto-merge on #{n}?"),
                    "",
                    "Disable auto-merge",
                    false,
                    Change {
                        label: format!("Disable auto-merge on #{n}"),
                        success: format!("Auto-merge disabled on #{n}"),
                        steps: vec![Step::Gql(graphql::disable_auto_merge(&d.pr.id))],
                        after: After::Pr(n),
                    },
                );
            }
        }
        Act::UpdateBranch(n) => {
            let Some(gh) = gh else { return };
            let st = &repo.devops.review;
            let Some(d) = loaded_detail(cx, st, n) else {
                return;
            };
            confirm_change(
                cx,
                format!("Update {} with {}?", d.pr.head, d.pr.base),
                format!(
                    "GitHub merges {} into {} (a merge commit on the branch). Refused if the head moved past {}.",
                    d.pr.base,
                    d.pr.head,
                    short(&d.pr.head_oid)
                ),
                "Update branch",
                false,
                Change {
                    label: format!("Update the branch of #{n}"),
                    success: format!("Updating {} with {}", d.pr.head, d.pr.base),
                    steps: vec![Step::Rest(rest::update_branch(&gh, n, &d.pr.head_oid))],
                    after: After::Pr(n),
                },
            );
        }
        Act::SetDraft { pr: n, draft } => {
            let st = &repo.devops.review;
            let Some(d) = loaded_detail(cx, st, n) else {
                return;
            };
            let (title, verb, m) = if draft {
                (
                    format!("Convert #{n} to a draft?"),
                    "Convert to draft",
                    graphql::convert_to_draft(&d.pr.id),
                )
            } else {
                (
                    format!("Mark #{n} as ready for review?"),
                    "Ready for review",
                    graphql::mark_ready_for_review(&d.pr.id),
                )
            };
            confirm_change(
                cx,
                title,
                if draft {
                    "Reviewers are no longer notified, and it cannot be merged until it is ready again."
                } else {
                    "Requested reviewers are notified."
                },
                verb,
                false,
                Change {
                    label: format!("{verb}: #{n}"),
                    success: if draft {
                        format!("#{n} is a draft")
                    } else {
                        format!("#{n} is ready for review")
                    },
                    steps: vec![Step::Gql(m)],
                    after: After::Pr(n),
                },
            );
        }
        Act::SetOpen { pr: n, open } => {
            let Some(gh) = gh else { return };
            let verb = if open { "Reopen" } else { "Close" };
            confirm_change(
                cx,
                format!("{verb} pull request #{n}?"),
                if open {
                    ""
                } else {
                    "It stays on GitHub and can be reopened."
                },
                verb,
                !open,
                Change {
                    label: format!("{verb} #{n}"),
                    success: format!("{} #{n}", if open { "Reopened" } else { "Closed" }),
                    steps: vec![Step::Rest(rest::set_pr_state(&gh, n, open))],
                    after: After::Pr(n),
                },
            );
        }
        Act::Checkout(n) => {
            let op = GitOp::GhPrCheckout { number: n };
            cx.confirm(
                format!("Check out pull request #{n}?"),
                "Fetches the pull request's branch and switches to it. Uncommitted changes stay in the working tree unless they conflict.",
                "Check out",
                false,
                op_details(&op),
                Act::RunOps(vec![op]).into(),
            );
        }
        Act::Run(change) => run_change(cx, &mut repo.devops.review, change),
        Act::RunOps(ops) => cx.ops(ops),
        Act::NewPr => {
            let meta = repo.devops.review.meta();
            let st = &mut repo.devops.review;
            st.mode = Mode::Prs;
            st.form_open = true;
            if st.meta.wanted() {
                fetch_meta(cx, st);
            }
            if st.suggestions.wanted() {
                fetch_suggestions(cx, st);
            }
            if st.new_pr.is_none() {
                repo.devops.review.new_pr = Some(new_pr_form(repo, &meta));
                // Templates now; the commits once the base is known.
                read_local(cx, Vec::new(), None);
                refresh_commits(cx, repo);
            }
        }
        Act::SetBase(base) => {
            if let Some(f) = repo.devops.review.new_pr.as_mut() {
                f.base = base;
            }
            refresh_commits(cx, repo);
        }
        Act::CloseForm => {
            let st = &mut repo.devops.review;
            match st.mode {
                Mode::Prs => st.form_open = false,
                Mode::Issues => st.issues.form_open = false,
            }
        }
        Act::DiscardForm => {
            let st = &mut repo.devops.review;
            match st.mode {
                Mode::Prs => {
                    st.form_open = false;
                    st.new_pr = None;
                }
                Mode::Issues => {
                    st.issues.form_open = false;
                    st.issues.form = None;
                }
            }
        }
        Act::GenerateBody => {
            if let Some(f) = repo.devops.review.new_pr.as_mut()
                && let Some(Ok(commits)) = &f.commits
            {
                let bullets = local::body_from_commits(commits);
                if f.body.trim().is_empty() {
                    f.body = bullets;
                } else {
                    if !f.body.ends_with('\n') {
                        f.body.push('\n');
                    }
                    f.body.push('\n');
                    f.body.push_str(&bullets);
                }
            }
        }
        Act::UseTemplate(i) => {
            if let Some(f) = repo.devops.review.new_pr.as_mut()
                && let Some(Ok(t)) = &f.templates
                && let Some(t) = t.get(i)
            {
                f.body = t.body.clone();
                f.template = Some(i);
            }
        }
        Act::CreatePr => {
            let Some(gh) = gh else { return };
            let st = &mut repo.devops.review;
            let Some(change) = new_pr_change(st, &gh) else {
                return;
            };
            let draft = st.new_pr.as_ref().is_some_and(|f| f.draft);
            let extra = change.steps.len() - 1;
            confirm_change(
                cx,
                format!(
                    "Open {} pull request on {}?",
                    if draft { "a draft" } else { "a" },
                    gh.full_name()
                ),
                if extra > 0 {
                    format!(
                        "{} more {} follow; {{number}} stands for the number GitHub gives the new pull request.",
                        extra,
                        if extra == 1 { "request" } else { "requests" }
                    )
                } else {
                    String::new()
                },
                "Open pull request",
                false,
                change,
            );
        }
        Act::PushFirst => match push_dialog(repo) {
            Some(d) => cx.action(Action::Dialog(d)),
            None => cx.toast(ToastKind::Info, "There is no branch or remote to push to."),
        },
        Act::LoadSuggestions => {
            let st = &mut repo.devops.review;
            if !st.suggestions.loading {
                st.suggestions.error = None;
                fetch_suggestions(cx, st);
            }
        }
        Act::OpenIssue(n) => select_issue(cx, &mut repo.devops.review, n),
        Act::OpenListedIssue(i) => {
            let st = &mut repo.devops.review;
            st.mode = Mode::Issues;
            match st.issues.visible().get(i).map(|x| x.number) {
                Some(n) => select_issue(cx, st, n),
                None => st.issues.pending_listed = Some(i),
            }
        }
        Act::SetIssueFilter(f) => {
            let st = &mut repo.devops.review;
            st.issues.filter = f;
            if st.issues.current().is_none_or(|l| l.wanted()) {
                fetch_issues(cx, st);
            }
        }
        Act::SetIssueLabel(l) => {
            let st = &mut repo.devops.review;
            st.issues.label = l;
            if st.issues.current().is_none_or(|l| l.wanted()) {
                fetch_issues(cx, st);
            }
        }
        Act::RefreshIssue(n) => fetch_issue(cx, &mut repo.devops.review, n),
        Act::IssueComment(n) => {
            let Some(gh) = gh else { return };
            let body = repo
                .devops
                .review
                .issues
                .comment
                .get(&n)
                .cloned()
                .unwrap_or_default();
            if body.trim().is_empty() {
                return;
            }
            confirm_change(
                cx,
                format!("Comment on issue #{n}?"),
                "Published right away.",
                "Comment",
                false,
                Change {
                    label: format!("Comment on issue #{n}"),
                    success: format!("Commented on issue #{n}"),
                    steps: vec![Step::Rest(rest::issue_comment(&gh, n, &body))],
                    after: After::IssueComment(n),
                },
            );
        }
        Act::SetIssueState {
            number: n,
            open,
            reason,
        } => {
            let Some(gh) = gh else { return };
            let title = if open {
                format!("Reopen issue #{n}?")
            } else {
                format!("{} #{n}?", reason.label())
            };
            let verb = if open { "Reopen" } else { reason.label() };
            confirm_change(
                cx,
                title,
                "",
                verb,
                false,
                Change {
                    label: format!("{} issue #{n}", if open { "Reopen" } else { "Close" }),
                    success: if open {
                        format!("Reopened issue #{n}")
                    } else {
                        format!("Closed issue #{n} as {}", reason.api().replace('_', " "))
                    },
                    steps: vec![Step::Rest(rest::set_issue_state(&gh, n, open, reason))],
                    after: After::Issue(n),
                },
            );
        }
        Act::EditIssue(n) => {
            let st = &mut repo.devops.review;
            if let Some(i) = st.issues.find(n).cloned() {
                st.issues.edit = Some(IssueEdit {
                    number: n,
                    labels: i.labels.iter().map(|l| l.name.clone()).collect(),
                    label_buf: String::new(),
                    assignees: i.assignees.clone(),
                    assignee_buf: String::new(),
                });
            }
            if st.suggestions.wanted() {
                fetch_suggestions(cx, st);
            }
        }
        Act::SaveIssueEdit(n) => {
            let Some(gh) = gh else { return };
            let st = &repo.devops.review;
            let Some(e) = st.issues.edit.as_ref().filter(|e| e.number == n) else {
                return;
            };
            confirm_change(
                cx,
                format!("Save the labels and assignees of #{n}?"),
                "Replaces both lists on GitHub with these.",
                "Save",
                false,
                Change {
                    label: format!("Edit issue #{n}"),
                    success: format!(
                        "Issue #{n}: {} and {}",
                        format::plural(e.labels.len(), "label", "labels"),
                        format::plural(e.assignees.len(), "assignee", "assignees")
                    ),
                    steps: vec![Step::Rest(rest::edit_issue(
                        &gh,
                        n,
                        &e.labels,
                        &e.assignees,
                    ))],
                    after: After::IssueEdit(n),
                },
            );
        }
        Act::StartBranch(n) => {
            let st = &repo.devops.review;
            let Some(b) = st.issues.branch.as_ref().filter(|b| b.number == n) else {
                return;
            };
            let name = b.name.trim().to_owned();
            if name.is_empty() {
                return;
            }
            let Some(start) = branch_start(repo, b.from_current) else {
                cx.toast(
                    ToastKind::Info,
                    "No start point: the default branch is not known yet, or HEAD is detached.",
                );
                return;
            };
            let op = GitOp::CreateBranch {
                name: name.clone(),
                start: start.clone(),
                switch: true,
            };
            cx.confirm(
                format!("Start {name} for issue #{n}?"),
                format!("Creates the branch at {start} and switches to it."),
                "Create branch",
                false,
                op_details(&op),
                Act::RunOps(vec![op]).into(),
            );
        }
        Act::NewIssue => {
            let st = &mut repo.devops.review;
            st.mode = Mode::Issues;
            st.issues.form_open = true;
            if st.suggestions.wanted() {
                fetch_suggestions(cx, st);
            }
            if st.issues.form.is_none() {
                st.issues.form = Some(NewIssueForm::default());
                read_local(cx, Vec::new(), None);
            }
        }
        Act::UseIssueTemplate(i) => {
            if let Some(f) = repo.devops.review.issues.form.as_mut()
                && let Some(t) = f.templates.as_ref().and_then(|t| t.get(i)).cloned()
            {
                f.body = t.body;
                if f.title.trim().is_empty() {
                    f.title = t.title;
                }
                for l in t.labels {
                    if !f.labels.contains(&l) {
                        f.labels.push(l);
                    }
                }
                for a in t.assignees {
                    if !f.assignees.contains(&a) {
                        f.assignees.push(a);
                    }
                }
                f.template = Some(i);
            }
        }
        Act::CreateIssue => {
            let Some(gh) = gh else { return };
            let Some(f) = repo.devops.review.issues.form.as_mut() else {
                return;
            };
            if f.title.trim().is_empty() {
                f.error = "Give the issue a title.".to_owned();
                return;
            }
            f.error.clear();
            let change = Change {
                label: "Open an issue".to_owned(),
                success: format!("Opened issue “{}”", f.title.trim()),
                steps: vec![Step::Rest(rest::create_issue(
                    &gh,
                    &f.title,
                    &f.body,
                    &f.labels,
                    &f.assignees,
                ))],
                after: After::IssueCreated,
            };
            confirm_change(
                cx,
                format!("Open an issue on {}?", gh.full_name()),
                "",
                "Open issue",
                false,
                change,
            );
        }
        Act::CheckGh => {
            hadion_core::devops::tools::refresh();
            let st = &mut repo.devops.review;
            st.gh_ok = Some(gh_installed());
            // Earlier failures were "gh not found": try again.
            st.meta = Fetch::default();
            st.open = Fetch::default();
            st.closed = Fetch::default();
            st.issues.lists.clear();
            on_page_shown(cx, repo);
        }
        Act::Script(cmd, arg) => script_act(cx, repo, &cmd, &arg),
    }
}

pub(super) fn script_act(cx: &mut Ctx, repo: &mut RepoState, cmd: &str, arg: &str) {
    let st = &mut repo.devops.review;
    let selected = st.selected;
    let mode = st.mode;
    match cmd {
        "review-filter" => match mode {
            Mode::Prs => {
                if let Some(f) = PrFilter::from_name(arg) {
                    apply(cx, repo, Act::SetFilter(f));
                }
            }
            Mode::Issues => {
                if let Some(f) = IssueFilter::from_name(arg) {
                    apply(cx, repo, Act::SetIssueFilter(f));
                }
            }
        },
        "review-scroll" => {
            if let Ok(y) = arg.trim().parse::<f32>() {
                view::request_scroll(y);
            }
        }
        "review-search" => match mode {
            Mode::Prs => st.search = arg.to_owned(),
            Mode::Issues => st.issues.search = arg.to_owned(),
        },
        "pr-lines" => {
            let mut it = arg
                .split_whitespace()
                .filter_map(|x| x.parse::<usize>().ok());
            let Some(a) = it.next() else {
                return;
            };
            let b = it.next().unwrap_or(a);
            let Some(n) = selected else { return };
            let v = st.prs.entry(n).or_default();
            let Some(path) = v.file.clone() else { return };
            let Some(file) = v
                .files
                .data
                .as_ref()
                .and_then(|f| f.iter().find(|f| f.path == path))
            else {
                return;
            };
            let hunks = patch::parse_patch(file.patch.as_deref().unwrap_or(""));
            let flat: Vec<patch::LinePos> = hunks
                .iter()
                .enumerate()
                .flat_map(|(h, hk)| {
                    (0..hk.lines.len()).map(move |l| patch::LinePos { hunk: h, line: l })
                })
                .collect();
            if let (Some(pa), Some(pb)) = (flat.get(a), flat.get(b)) {
                v.sel = Some(LineSel {
                    path,
                    anchor: *pa,
                    end: *pb,
                });
            }
        }
        "pr-compose" | "pr-comment" => {
            let Some(n) = selected else { return };
            st.prs.entry(n).or_default().composer = arg.replace("\\n", "\n");
            if cmd == "pr-comment" {
                apply(cx, repo, Act::AddPending(n));
            }
        }
        "pr-suggest" => {
            if let Some(n) = selected {
                suggest(st, n);
            }
        }
        "pr-review" => {
            let Some(n) = selected else { return };
            let v = st.prs.entry(n).or_default();
            v.review_open = true;
            let (event, body) = arg.split_once(' ').unwrap_or((arg, ""));
            v.review_event = match event {
                "approve" => ReviewEvent::Approve,
                "changes" => ReviewEvent::RequestChanges,
                _ => ReviewEvent::Comment,
            };
            if !body.is_empty() {
                v.review_body = body.to_owned();
            }
        }
        "pr-submit" => {
            if let Some(n) = selected {
                apply(cx, repo, Act::SubmitReview(n));
            }
        }
        "pr-merge" => {
            let Some(n) = selected else { return };
            if let Some(m) = MergeMethod::parse(arg) {
                st.prs.entry(n).or_default().merge.method = Some(m);
            }
            apply(cx, repo, Act::Merge(n));
        }
        "pr-next-thread" => {
            if let Some(n) = selected {
                files::jump_next(st.prs.entry(n).or_default());
            }
        }
        "pr-threads" => {
            // Open every thread of the selected pull request.
            let Some(n) = selected else { return };
            let v = st.prs.entry(n).or_default();
            if let Some(d) = v.detail.data.as_ref() {
                v.opened = d.threads.iter().map(|t| t.id.clone()).collect();
                v.closed.clear();
            }
        }
        "create-pr" => apply(cx, repo, Act::CreatePr),
        "pr-title" => {
            if let Some(f) = st.new_pr.as_mut() {
                f.title = arg.to_owned();
                f.title_edited = true;
            }
        }
        "generate-body" => apply(cx, repo, Act::GenerateBody),
        "start-branch" => {
            let Some(n) = st.issues.selected else { return };
            if st.issues.branch.as_ref().is_none_or(|b| b.number != n) {
                let title = st
                    .issues
                    .find(n)
                    .map(|i| i.title.clone())
                    .unwrap_or_default();
                st.issues.branch = Some(BranchForm {
                    number: n,
                    name: local::issue_branch_name(n, &title),
                    from_current: arg == "current",
                });
            }
            apply(cx, repo, Act::StartBranch(n));
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let st = &mut repo.devops.review;
    match msg {
        Msg::Meta(r) => {
            st.meta.finish(*r);
            fill_base(cx, repo);
        }
        Msg::List {
            kind,
            append,
            result,
        } => {
            if append {
                st.more_loading = false;
                match *result {
                    Ok(page) => {
                        if let Some(l) = st.list_mut(kind).data.as_mut() {
                            for item in page.items {
                                if !l.items.iter().any(|x| x.number == item.number) {
                                    l.items.push(item);
                                }
                            }
                            l.end_cursor = page.end_cursor;
                            l.has_more = page.has_more;
                            l.total = page.total;
                            l.light |= page.light;
                        }
                    }
                    Err(e) => cx.toast(ToastKind::Error, format!("Could not load more: {e}")),
                }
            } else {
                st.list_mut(kind).finish(*result);
                fill_base(cx, repo);
            }
            let st = &mut repo.devops.review;
            if let Some(i) = st.pending_listed
                && kind == st.filter.kind()
                && let Some(n) = st.visible().get(i).map(|p| p.number)
            {
                st.pending_listed = None;
                select_pr(cx, repo, n);
            }
        }
        Msg::Detail(n, r) => {
            let r = *r;
            let fresh = r.as_ref().ok().map(|d| d.pr.clone());
            st.prs.entry(n).or_default().detail.finish(r);
            // Keep the list rows in step with the fresher detail.
            if let Some(pr) = fresh {
                for f in [&mut st.open, &mut st.closed] {
                    if let Some(l) = f.data.as_mut() {
                        for item in l
                            .items
                            .iter_mut()
                            .chain(l.requested.iter_mut())
                            .chain(l.mine.iter_mut())
                        {
                            if item.number == n {
                                *item = pr.clone();
                            }
                        }
                    }
                }
            }
            if st.selected == Some(n) && st.tab == PrTab::Checks {
                tab_shown(cx, repo);
            }
        }
        Msg::Files(n, r) => {
            let selected = st.selected;
            let pending = st.pending_file;
            let v = st.prs.entry(n).or_default();
            v.files.finish(r);
            v.parsed.clear();
            if let Some(files) = v.files.data.as_ref() {
                let keep = v
                    .file
                    .as_ref()
                    .is_some_and(|p| files.iter().any(|f| &f.path == p));
                if let Some(i) = pending.filter(|_| selected == Some(n)) {
                    v.file = files.get(i).map(|f| f.path.clone());
                    st.pending_file = None;
                } else if !keep {
                    v.file = files.first().map(|f| f.path.clone());
                }
            }
        }
        Msg::Commits(n, r) => st.prs.entry(n).or_default().commits.finish(r),
        Msg::Suggestions(r) => st.suggestions.finish(*r),
        Msg::Local(info) => {
            let info = *info;
            if let Some(f) = st.new_pr.as_mut() {
                if info.commits.is_some() {
                    f.base_ref = info.base_ref.clone();
                }
                if f.body.is_empty()
                    && let Some(t) = info.templates.first()
                {
                    f.body = t.body.clone();
                    f.template = Some(0);
                }
                if !f.title_edited {
                    match &info.commits {
                        Some(Ok(c)) => f.title = local::default_title(c, &f.head),
                        _ if f.title.is_empty() => f.title = local::humanize_branch(&f.head),
                        _ => {}
                    }
                }
                f.templates = Some(Ok(info.templates.clone()));
                if info.commits.is_some() {
                    f.commits = info.commits.clone();
                }
            }
            if let Some(f) = st.issues.form.as_mut() {
                f.templates = Some(info.issue_templates);
            }
        }
        Msg::Issues { key, login, result } => {
            if login.is_some() && st.issues.login.is_none() {
                st.issues.login = login;
            }
            st.issues
                .lists
                .entry(key.clone())
                .or_default()
                .finish(result);
            if key == st.issues.key()
                && let Some(i) = st.issues.pending_listed
                && let Some(n) = st.issues.visible().get(i).map(|x| x.number)
            {
                st.issues.pending_listed = None;
                select_issue(cx, st, n);
            }
        }
        Msg::Issue(n, r) => st.issues.details.entry(n).or_default().finish(*r),
        Msg::Changed { after, ok, number } => {
            st.changing = None;
            match after {
                After::Pr(n) => fetch_detail(cx, st, n),
                After::Review(n) => {
                    if ok {
                        let v = st.prs.entry(n).or_default();
                        v.pending.clear();
                        v.review_body.clear();
                        v.review_open = false;
                        v.review_event = ReviewEvent::Comment;
                    }
                    fetch_detail(cx, st, n);
                }
                After::Comment(n) => {
                    if ok {
                        st.prs.entry(n).or_default().comment.clear();
                    }
                    fetch_detail(cx, st, n);
                }
                After::LineComment(n) => {
                    if ok {
                        let v = st.prs.entry(n).or_default();
                        v.composer.clear();
                        v.sel = None;
                    }
                    fetch_detail(cx, st, n);
                }
                After::Reply(n, thread) => {
                    if ok {
                        st.prs.entry(n).or_default().replies.remove(&thread);
                    }
                    fetch_detail(cx, st, n);
                }
                After::Created => {
                    if ok {
                        st.new_pr = None;
                        st.form_open = false;
                        st.filter = PrFilter::Open;
                        fetch_list(cx, st, ListKind::Open, false);
                        if let Some(n) = number {
                            st.tab = PrTab::Conversation;
                            select_pr(cx, repo, n);
                        }
                    }
                }
                After::Issue(n) => {
                    fetch_issue(cx, st, n);
                    st.issues.lists.retain(|_, f| f.loading);
                    fetch_issues(cx, st);
                }
                After::IssueComment(n) => {
                    if ok {
                        st.issues.comment.remove(&n);
                    }
                    fetch_issue(cx, st, n);
                }
                After::IssueEdit(n) => {
                    if ok {
                        st.issues.edit = None;
                    }
                    fetch_issue(cx, st, n);
                }
                After::IssueCreated => {
                    if ok {
                        st.issues.form = None;
                        st.issues.form_open = false;
                        st.issues.filter = IssueFilter::Open;
                        st.issues.lists.retain(|_, f| f.loading);
                        fetch_issues(cx, st);
                        if let Some(n) = number {
                            select_issue(cx, st, n);
                        }
                    }
                }
            }
        }
    }
}
