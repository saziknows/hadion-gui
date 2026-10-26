//! Changes of the Review page: building them (merge, review, comments,
//! new pull request), confirming them with every request and body, and
//! running their steps in order.

use super::*;

pub(super) fn confirm_change(
    cx: &mut Ctx,
    title: impl Into<String>,
    body: impl Into<String>,
    confirm: &str,
    danger: bool,
    change: Change,
) {
    let details = change.steps.iter().map(Step::confirmation).collect();
    cx.confirm(
        title,
        body,
        confirm,
        danger,
        details,
        Act::Run(change).into(),
    );
}

pub(super) fn run_change(cx: &mut Ctx, st: &mut State, change: Change) {
    if let Some(running) = &st.changing {
        cx.toast(
            ToastKind::Info,
            format!("Wait for “{running}” to finish first."),
        );
        return;
    }
    let Change {
        label,
        success,
        steps,
        after,
    } = change;
    st.changing = Some(label.clone());
    let started = cx.gh_change(label, move |gh, log| {
        let out = rest::run_steps(gh, &steps, &mut log.calls);
        let ok = out.error.is_none();
        let result = match &out.error {
            None => Ok(success),
            Some((i, e)) => {
                let mut msg = String::new();
                if steps.len() > 1 {
                    msg.push_str(&format!("Request {} of {} failed: ", i + 1, steps.len()));
                }
                msg.push_str(e);
                if out.done > 0 {
                    let done: Vec<String> = steps[..out.done].iter().map(Step::display).collect();
                    msg.push_str(&format!("\nAlready done: {}", done.join("; ")));
                }
                Err(msg)
            }
        };
        (
            result,
            Some(Msg::Changed {
                after,
                ok,
                number: out.number,
            }),
        )
    });
    if !started {
        st.changing = None;
    }
}

/// The detail of `n`, or a toast saying it is not loaded.
pub(super) fn loaded_detail(cx: &mut Ctx, st: &State, n: u64) -> Option<PrDetail> {
    let d = st.prs.get(&n).and_then(|v| v.detail.data.clone());
    if d.is_none() {
        cx.toast(
            ToastKind::Info,
            format!("Pull request #{n} is still loading."),
        );
    }
    d
}

pub(super) fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// The comment the composer would send, anchored on the selected lines.
pub(super) fn composed(st: &State, n: u64) -> Result<DraftComment, String> {
    let v = st.prs.get(&n).ok_or("Nothing selected.")?;
    let sel = v.sel.as_ref().ok_or("Select a line first.")?;
    let file = v
        .files
        .data
        .as_ref()
        .and_then(|f| f.iter().find(|f| f.path == sel.path))
        .ok_or("The file is not loaded.")?;
    let hunks = patch::parse_patch(file.patch.as_deref().unwrap_or(""));
    let (a, b) = sel.range();
    let (start, end) = patch::range_anchor(&hunks, a, b).map_err(str::to_owned)?;
    let body = v.composer.trim_end().to_owned();
    if body.trim().is_empty() {
        return Err("Write the comment first.".to_owned());
    }
    Ok(DraftComment {
        path: sel.path.clone(),
        line: end.1,
        side: end.0,
        start_line: start.map(|s| s.1),
        start_side: start.map(|s| s.0),
        body,
    })
}

pub(super) fn merge_change(
    cx: &mut Ctx,
    st: &State,
    gh: &Gh,
    n: u64,
    auto: bool,
) -> Option<Change> {
    let d = loaded_detail(cx, st, n)?;
    let meta = st.meta();
    let form = &st.prs.get(&n)?.merge;
    let method = form
        .method
        .or(meta.preferred_method())
        .unwrap_or(MergeMethod::Merge);
    let title = method.has_message().then_some(form.title.as_str());
    let message = method.has_message().then_some(form.message.as_str());
    let mut steps = Vec::new();
    if auto {
        steps.push(Step::Gql(graphql::enable_auto_merge(
            &d.pr.id,
            method,
            title,
            message,
            &d.pr.head_oid,
        )));
    } else {
        steps.push(Step::Rest(rest::merge_pr(
            gh,
            n,
            method,
            &d.pr.head_oid,
            title,
            message,
        )));
        let repo_deletes = meta.delete_branch_on_merge == Some(true);
        let delete = form.delete_branch.unwrap_or(repo_deletes);
        if delete && !repo_deletes && d.pr.head_in_repo(&gh.full_name()) {
            steps.push(Step::Rest(rest::delete_branch(gh, &d.pr.head)));
        }
    }
    Some(Change {
        label: if auto {
            format!("Enable auto-merge on #{n}")
        } else {
            format!("Merge #{n}")
        },
        success: if auto {
            format!(
                "#{n} will be merged ({}) once its requirements are met",
                method.rest()
            )
        } else {
            format!("Merged #{n} into {} ({})", d.pr.base, method.rest())
        },
        steps,
        after: After::Pr(n),
    })
}

pub(super) fn new_pr_change(st: &mut State, gh: &Gh) -> Option<Change> {
    let f = st.new_pr.as_mut()?;
    f.error.clear();
    if f.title.trim().is_empty() {
        f.error = "Give the pull request a title.".to_owned();
        return None;
    }
    if f.head_ref.is_empty() || f.base.is_empty() {
        f.error = "Choose the base and head branches.".to_owned();
        return None;
    }
    if f.head == f.base {
        f.error = format!("{} cannot be merged into itself.", f.head);
        return None;
    }
    let mut steps = vec![Step::Rest(rest::create_pr(
        gh,
        &rest::NewPr {
            title: f.title.clone(),
            head: f.head_ref.clone(),
            base: f.base.clone(),
            body: f.body.clone(),
            draft: f.draft,
        },
    ))];
    if !f.labels.is_empty() {
        steps.push(Step::Rest(rest::add_labels(gh, Num::Created, &f.labels)));
    }
    if !f.reviewers.is_empty() || !f.teams.is_empty() {
        steps.push(Step::Rest(rest::request_reviewers(
            gh,
            Num::Created,
            &f.reviewers,
            &f.teams,
        )));
    }
    if !f.assignees.is_empty() {
        steps.push(Step::Rest(rest::add_assignees(
            gh,
            Num::Created,
            &f.assignees,
        )));
    }
    Some(Change {
        label: format!("Open pull request {} → {}", f.head, f.base),
        success: format!(
            "Opened {} pull request {} → {}",
            if f.draft { "a draft" } else { "a" },
            f.head,
            f.base
        ),
        steps,
        after: After::Created,
    })
}

/// Defaults of the New pull request form from the repository.
pub(super) fn new_pr_form(repo: &RepoState, meta: &RepoMeta) -> NewPrForm {
    let snap = repo.snapshot.as_ref();
    let current = snap.and_then(|s| s.current_branch());
    let head = current.map(|b| b.name.clone()).unwrap_or_default();
    let mut head_ref = head.clone();
    if let (Some(b), Some(snap)) = (current, snap)
        && let Some(up) = b.upstream.as_deref().filter(|_| !b.upstream_gone)
        && let Some(rb) = snap.remote_branch(up)
    {
        head_ref = rb.branch.clone();
        let owner = snap
            .remotes
            .iter()
            .find(|r| r.name == rb.remote)
            .and_then(|r| gitvis_core::github::parse_remote(&r.url))
            .map(|(o, _)| o);
        if let (Some(owner), Some((repo_owner, _))) = (owner, repo.github_remote.as_ref())
            && !owner.eq_ignore_ascii_case(repo_owner)
        {
            head_ref = format!("{owner}:{head_ref}");
        }
    }
    NewPrForm {
        base: meta.default_branch.clone(),
        head,
        head_ref,
        ..Default::default()
    }
}

/// Where `git log base..head` looks for the base: the GitHub remote's
/// branch first, then a local one.
pub(super) fn base_candidates(repo: &RepoState, base: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(snap) = repo.snapshot.as_ref() {
        for r in &snap.remotes {
            if gitvis_core::github::parse_remote(&r.url).as_ref() == repo.github_remote.as_ref() {
                out.push(format!("{}/{base}", r.name));
            }
        }
    }
    out.push(format!("origin/{base}"));
    out.push(base.to_owned());
    let mut seen = HashSet::new();
    out.retain(|c| seen.insert(c.clone()));
    out
}

/// The push dialog for the current branch (as the toolbar opens it).
pub(super) fn push_dialog(repo: &RepoState) -> Option<Dialog> {
    let snap = repo.snapshot.as_ref()?;
    let b = snap.current_branch()?;
    let remotes: Vec<String> = snap.remotes.iter().map(|r| r.name.clone()).collect();
    if remotes.is_empty() {
        return None;
    }
    let upstream_remote = b
        .upstream
        .as_deref()
        .filter(|_| !b.upstream_gone)
        .and_then(|u| snap.remote_branch(u))
        .map(|rb| rb.remote.clone());
    let remote = upstream_remote
        .as_ref()
        .and_then(|r| remotes.iter().position(|x| x == r))
        .or_else(|| remotes.iter().position(|r| r == "origin"))
        .unwrap_or(0);
    Some(Dialog::Push {
        branch: b.name.clone(),
        remotes,
        remote,
        set_upstream: upstream_remote.is_none(),
        force: false,
    })
}

/// The git operation's command lines, for a confirmation.
pub(super) fn op_details(op: &GitOp) -> Vec<String> {
    op.commands()
        .iter()
        .map(|c| format!("{} {}", op.program(), c.join(" ")))
        .collect()
}

/// Add a ```suggestion block of the selected lines to the composer.
pub(super) fn suggest(st: &mut State, n: u64) {
    let Some(v) = st.prs.get_mut(&n) else {
        return;
    };
    let Some(sel) = v.sel.clone() else {
        return;
    };
    let Some(file) = v
        .files
        .data
        .as_ref()
        .and_then(|f| f.iter().find(|f| f.path == sel.path))
    else {
        return;
    };
    let hunks = patch::parse_patch(file.patch.as_deref().unwrap_or(""));
    let (a, b) = sel.range();
    match patch::suggestion_lines(&hunks, a, b) {
        Some(text) => {
            if !v.composer.is_empty() && !v.composer.ends_with('\n') {
                v.composer.push('\n');
            }
            v.composer.push_str(&patch::suggestion_block(&text));
            v.composer_error.clear();
        }
        None => {
            v.composer_error =
                "Suggestions replace lines of the new version: select added or unchanged lines."
                    .to_owned()
        }
    }
}

/// Where a branch for an issue starts: the default branch (local if it
/// exists, else the GitHub remote's), or the current branch.
pub(super) fn branch_start(repo: &RepoState, from_current: bool) -> Option<String> {
    let snap = repo.snapshot.as_ref()?;
    if from_current {
        return snap
            .current_branch()
            .map(|b| b.name.clone())
            .or_else(|| snap.head.target().map(|t| t.to_string()));
    }
    let base = repo.devops.review.meta().default_branch;
    if base.is_empty() {
        return None;
    }
    if snap.branch(&base).is_some() {
        return Some(base);
    }
    base_candidates(repo, &base)
        .into_iter()
        .find(|c| snap.remote_branch(c).is_some())
}
