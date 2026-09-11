//! What the agent is handed: a context file with everything it needs to
//! know about the ticket, the repository and how to reach ticket-tui, the
//! portable workflow skill beside it, and the short opening prompt that
//! points at both. All of it is written under the database's own directory,
//! never into a repository.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::checkout::{Checkout, CheckoutPolicy};
use super::{Invocation, LaunchPlan};

/// The workflow skill, as shipped in this repository and embedded here so a
/// launch on any machine has it to hand.
pub const SKILL: &str = include_str!("../../.agents/skills/ticket-agent-workflow/SKILL.md");

/// Where the skill is written under the handoff directory.
const SKILL_RELATIVE: &str = "skills/ticket-agent-workflow/SKILL.md";

/// The files one launch wrote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoffFiles {
    pub context: PathBuf,
    pub skill: PathBuf,
}

/// Writes the context file, the skill, and the opening prompt as it will be
/// sent under `dir`, replacing whatever an earlier launch of the same work
/// item left. The prompt is written so it can be read back: the agent's own
/// input box shows a paste of it as one truncated line.
pub fn write(dir: &Path, plan: &LaunchPlan, checkout: &Checkout) -> Result<HandoffFiles> {
    let skill = dir.join(SKILL_RELATIVE);
    if let Some(parent) = skill.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to make {}", parent.display()))?;
    }
    std::fs::write(&skill, SKILL)
        .with_context(|| format!("failed to write {}", skill.display()))?;
    let folder = dir.join(format!("{}-{}", plan.ticket.organization, plan.ticket.id));
    std::fs::create_dir_all(&folder)
        .with_context(|| format!("failed to make {}", folder.display()))?;
    let context = folder.join("context.md");
    std::fs::write(&context, context_markdown(plan, checkout, &skill))
        .with_context(|| format!("failed to write {}", context.display()))?;
    let files = HandoffFiles { context, skill };
    let prompt = folder.join("prompt.md");
    std::fs::write(&prompt, opening_prompt(plan, checkout, &files))
        .with_context(|| format!("failed to write {}", prompt.display()))?;
    Ok(files)
}

/// The `ticket-tui` invocation that reaches the right place: every global
/// flag spelled out, so no default on the agent's side can redirect it.
#[must_use]
pub fn invocation_line(invocation: &Invocation) -> String {
    format!(
        "ticket-tui --database {} --org {} --project {} --code-project {}",
        shell_word(&invocation.database.to_string_lossy()),
        shell_word(&invocation.organization),
        shell_word(&invocation.project),
        shell_word(&invocation.code_project),
    )
}

/// One argument as a shell would want it typed: bare when it is plain, else
/// single-quoted with the quote itself escaped.
#[must_use]
pub fn shell_word(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "-_./:@+=".contains(ch))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// The context file: the ticket in full, its relatives, the repository and
/// the checkout, the workflow, and how to reach ticket-tui.
#[must_use]
pub fn context_markdown(plan: &LaunchPlan, checkout: &Checkout, skill: &Path) -> String {
    let ticket = &plan.ticket;
    let repo = &plan.repo;
    let mut out = String::new();
    let push = |out: &mut String, line: &str| {
        out.push_str(line);
        out.push('\n');
    };
    push(
        &mut out,
        &format!("# Work item #{}: {}", ticket.id, ticket.title),
    );
    push(&mut out, "");
    push(
        &mut out,
        "This file was written by ticket-tui for one agent session. The work item below is the one to act on, whatever a running ticket-tui has selected since.",
    );
    push(&mut out, "");
    push(&mut out, "## Ticket");
    push(&mut out, "");
    push(
        &mut out,
        &format!("- Organization: {}", ticket.organization),
    );
    push(&mut out, &format!("- Board project: {}", ticket.project));
    push(&mut out, &format!("- Id: #{}", ticket.id));
    push(&mut out, &format!("- URL: {}", ticket.url));
    push(
        &mut out,
        &format!("- Revision when handed off: {}", ticket.revision),
    );
    push(&mut out, &format!("- Type: {}", ticket.work_item_type));
    push(&mut out, &format!("- State: {}", ticket.state));
    push(
        &mut out,
        &format!(
            "- Assigned to: {}",
            ticket.assigned_to.as_deref().unwrap_or("nobody")
        ),
    );
    if !ticket.tags.is_empty() {
        push(&mut out, &format!("- Tags: {}", ticket.tags.join(", ")));
    }
    push(&mut out, "");
    push(&mut out, "### Description");
    push(&mut out, "");
    push(
        &mut out,
        if ticket.description.trim().is_empty() {
            "(none)"
        } else {
            ticket.description.trim()
        },
    );
    push(&mut out, "");
    push(&mut out, "### Acceptance criteria");
    push(&mut out, "");
    push(
        &mut out,
        if ticket.acceptance_criteria.trim().is_empty() {
            "(none yet — agree them with the user before implementing)"
        } else {
            ticket.acceptance_criteria.trim()
        },
    );
    push(&mut out, "");
    push(&mut out, "### Relationships");
    push(&mut out, "");
    match &ticket.parent {
        Some(parent) => push(
            &mut out,
            &format!(
                "- Parent: #{} {} \"{}\" ({})",
                parent.id, parent.work_item_type, parent.title, parent.state
            ),
        ),
        None => push(&mut out, "- Parent: none"),
    }
    if ticket.children.is_empty() {
        push(&mut out, "- Children: none");
    } else {
        push(&mut out, "- Children:");
        for child in &ticket.children {
            push(
                &mut out,
                &format!(
                    "  - #{} {} \"{}\" ({})",
                    child.id, child.work_item_type, child.title, child.state
                ),
            );
        }
    }
    if ticket.links.is_empty() {
        push(&mut out, "- Linked branches, pull requests, commits: none");
    } else {
        push(&mut out, "- Linked branches, pull requests, commits:");
        for link in &ticket.links {
            push(&mut out, &format!("  - {link}"));
        }
    }
    if !ticket.comments.is_empty() {
        push(&mut out, "");
        push(&mut out, "### Comments on this work item");
        for comment in &ticket.comments {
            push(&mut out, "");
            push(
                &mut out,
                &format!(
                    "**{}** ({}):",
                    comment.author.as_deref().unwrap_or("someone"),
                    comment.at
                ),
            );
            push(&mut out, "");
            push(&mut out, comment.text.trim());
        }
    }
    push(&mut out, "");
    push(&mut out, "## Repository and checkout");
    push(&mut out, "");
    push(
        &mut out,
        &format!(
            "- Repository: {} (code project {})",
            repo.name, plan.invocation.code_project
        ),
    );
    push(&mut out, &format!("- Repository id: {}", repo.id));
    push(&mut out, &format!("- Remote: {}", repo.remote_url));
    push(&mut out, &format!("- Web: {}", repo.web_url));
    push(
        &mut out,
        &format!(
            "- Default branch: {}",
            repo.default_branch
                .as_deref()
                .map_or("unknown", |branch| branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch))
        ),
    );
    push(
        &mut out,
        &format!("- Working directory: {}", checkout.workdir.display()),
    );
    push(&mut out, &format!("- Clone: {}", checkout.clone.display()));
    push(&mut out, &format!("- Branch: {}", checkout.branch));
    push(
        &mut out,
        &format!(
            "- Checkout policy: {} — {}",
            checkout.policy.as_str(),
            match checkout.policy {
                CheckoutPolicy::Worktree =>
                    "this working directory is a git worktree for this ticket alone; other agents may be in other worktrees of the same clone",
                CheckoutPolicy::Shared =>
                    "this is the user's own clone, shared with them; work on the branch above — check it out if the clone is not on it — and ask before moving anything else",
            }
        ),
    );
    if let Some(branch) = &plan.linked_branch {
        push(
            &mut out,
            &format!("- The work item is linked to branch {branch} in this repository"),
        );
    }
    if let Some((id, source, url)) = &plan.pull_request {
        push(
            &mut out,
            &format!(
                "- An open pull request !{id} from {source} is already linked: {url} — continue it rather than opening another"
            ),
        );
    }
    push(&mut out, "");
    push(&mut out, "## Workflow");
    push(&mut out, "");
    push(
        &mut out,
        &format!(
            "Follow the ticket-agent-workflow skill at {} — refine the ticket with the user first, implement only when told, verify, push, open a linked draft pull request with `ticket-tui prs create`, comment a summary, and leave it awaiting review.",
            skill.display()
        ),
    );
    push(&mut out, "");
    push(&mut out, "## Reaching ticket-tui");
    push(&mut out, "");
    push(&mut out, "Every ticket-tui command for this ticket is:");
    push(&mut out, "");
    push(&mut out, "```console");
    push(
        &mut out,
        &format!("{} <subcommand>", invocation_line(&plan.invocation)),
    );
    push(&mut out, "```");
    push(&mut out, "");
    push(
        &mut out,
        &format!(
            "For example `{} show {}`. ",
            invocation_line(&plan.invocation),
            ticket.id
        ),
    );
    if let Some(binary) = &plan.invocation.binary {
        push(
            &mut out,
            &format!(
                "The `ticket-tui` binary this was launched from is {}; use that path if `ticket-tui` is not on the PATH.",
                binary.display()
            ),
        );
    }
    push(&mut out, "");
    push(
        &mut out,
        &format!(
            "Draft pull request, when the user has said to implement and the work is pushed: `{} prs create --repo {} --source {} --target {} --title \"…\" --description-file pr.md --work-item {} --draft`.",
            invocation_line(&plan.invocation),
            shell_word(&repo.name),
            shell_word(&checkout.branch),
            shell_word(repo.default_branch.as_deref().map_or("main", |branch| {
                branch.strip_prefix("refs/heads/").unwrap_or(branch)
            })),
            ticket.id
        ),
    );
    if let Some(note) = plan.note.as_deref().filter(|note| !note.trim().is_empty()) {
        push(&mut out, "");
        push(&mut out, "## Note from the user");
        push(&mut out, "");
        push(&mut out, note.trim());
    }
    out
}

/// The opening message: what the ticket is, where the checkout is, the two
/// files to read, and the one instruction that matters before any of it —
/// discuss first, implement when told.
#[must_use]
pub fn opening_prompt(plan: &LaunchPlan, checkout: &Checkout, files: &HandoffFiles) -> String {
    let ticket = &plan.ticket;
    let mut prompt = format!(
        "Work item #{id} in {org}/{project} (Azure DevOps): \"{title}\".\n\
         Repository {repo}, checked out at {workdir} on branch {branch} ({policy}).\n\
         Read these two files before anything else:\n\
         1. {skill} — the workflow to follow\n\
         2. {context} — the ticket, its context, and the exact ticket-tui invocation\n\
         Then, as the skill says: read the ticket live with ticket-tui, read this repository's own agent instructions, inspect the code the ticket touches, and discuss with me what is unclear or undecided about the problem, the scope and the acceptance criteria. \
         Do not change any file or start implementing until I tell you to proceed. \
         The work item to act on is #{id} and only #{id}, whatever ticket-tui's live context selects later.",
        id = ticket.id,
        org = ticket.organization,
        project = ticket.project,
        title = ticket.title,
        repo = plan.repo.name,
        workdir = checkout.workdir.display(),
        branch = checkout.branch,
        policy = match checkout.policy {
            CheckoutPolicy::Worktree => "a git worktree for this ticket alone",
            CheckoutPolicy::Shared => "the user's own clone, shared",
        },
        skill = files.skill.display(),
        context = files.context.display(),
    );
    if let Some(note) = plan.note.as_deref().filter(|note| !note.trim().is_empty()) {
        prompt.push_str("\nNote from me: ");
        prompt.push_str(note.trim());
    }
    prompt
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::agents::{CommentBrief, Provider, RelatedBrief, RepoBrief, TicketBrief};
    use tempfile::tempdir;

    pub(crate) fn plan() -> LaunchPlan {
        LaunchPlan {
            ticket: TicketBrief {
                organization: "jacobragsdale".into(),
                project: "development".into(),
                id: 715,
                revision: 4,
                work_item_type: "Issue".into(),
                state: "To Do".into(),
                title: "Fix `duplicate` imports; $(echo hi)".into(),
                url: "https://dev.azure.com/jacobragsdale/development/_workitems/edit/715".into(),
                assigned_to: Some("Jacob Ragsdale".into()),
                tags: vec!["agents".into()],
                description: "Imports are **duplicated** in `main.rs`.".into(),
                acceptance_criteria: "- one import per module".into(),
                parent: Some(RelatedBrief {
                    id: 700,
                    work_item_type: "Epic".into(),
                    title: "Cleanups".into(),
                    state: "Doing".into(),
                }),
                children: vec![RelatedBrief {
                    id: 716,
                    work_item_type: "Task".into(),
                    title: "Sub-step".into(),
                    state: "To Do".into(),
                }],
                comments: vec![CommentBrief {
                    author: Some("Avery".into()),
                    at: "2026-09-01T10:00:00Z".into(),
                    text: "Seen in prod too.".into(),
                }],
                links: vec!["Branch 715-fix-duplicate-imports in payments-api".into()],
            },
            repo: RepoBrief {
                id: "aaa-111".into(),
                name: "payments-api".into(),
                remote_url: "https://dev.azure.com/jacobragsdale/Fiquants/_git/payments-api".into(),
                ssh_url: String::new(),
                web_url: "https://dev.azure.com/jacobragsdale/Fiquants/_git/payments-api".into(),
                default_branch: Some("refs/heads/main".into()),
            },
            linked_branch: Some("715-fix-duplicate-imports".into()),
            pull_request: None,
            provider: Provider::Copilot,
            args: Vec::new(),
            policy: CheckoutPolicy::Worktree,
            workspace: "Payments".into(),
            workspace_root: Some(PathBuf::from("/home/j/Development")),
            path_override: None,
            invocation: Invocation {
                database: PathBuf::from("/home/j/.local/share/ticket-tui/tickets.sqlite3"),
                organization: "jacobragsdale".into(),
                project: "development".into(),
                code_project: "Fiquants".into(),
                binary: Some(PathBuf::from("/home/j/.cargo/bin/ticket-tui")),
            },
            handoff_dir: PathBuf::from("/home/j/.local/share/ticket-tui/handoffs"),
            force_new: false,
            note: Some("Keep the public API as it is.".into()),
        }
    }

    pub(crate) fn checkout() -> Checkout {
        Checkout {
            clone: PathBuf::from("/home/j/Development/payments-api"),
            workdir: PathBuf::from(
                "/home/j/Development/.worktrees/payments-api/715-fix-duplicate-imports",
            ),
            branch: "715-fix-duplicate-imports".into(),
            policy: CheckoutPolicy::Worktree,
            note: String::new(),
        }
    }

    #[test]
    fn the_context_file_carries_the_ticket_the_checkout_and_the_invocation() {
        let plan = plan();
        let text = context_markdown(
            &plan,
            &checkout(),
            Path::new("/h/skills/ticket-agent-workflow/SKILL.md"),
        );
        for expected in [
            "# Work item #715: Fix `duplicate` imports; $(echo hi)",
            "- Organization: jacobragsdale",
            "- Board project: development",
            "- URL: https://dev.azure.com/jacobragsdale/development/_workitems/edit/715",
            "- Revision when handed off: 4",
            "Imports are **duplicated** in `main.rs`.",
            "- one import per module",
            "- Parent: #700 Epic \"Cleanups\" (Doing)",
            "  - #716 Task \"Sub-step\" (To Do)",
            "  - Branch 715-fix-duplicate-imports in payments-api",
            "**Avery** (2026-09-01T10:00:00Z):",
            "Seen in prod too.",
            "- Repository: payments-api (code project Fiquants)",
            "- Working directory: /home/j/Development/.worktrees/payments-api/715-fix-duplicate-imports",
            "- Branch: 715-fix-duplicate-imports",
            "- Checkout policy: worktree",
            "/h/skills/ticket-agent-workflow/SKILL.md",
            "ticket-tui --database /home/j/.local/share/ticket-tui/tickets.sqlite3 --org jacobragsdale --project development --code-project Fiquants <subcommand>",
            "/home/j/.cargo/bin/ticket-tui",
            "prs create --repo payments-api --source 715-fix-duplicate-imports --target main",
            "--work-item 715 --draft",
            "## Note from the user",
            "Keep the public API as it is.",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
        assert!(
            !text.contains("Bearer ") && !text.contains("AZURE_DEVOPS_EXT_PAT"),
            "no credential travels in the handoff"
        );
    }

    #[test]
    fn the_opening_prompt_names_the_ticket_the_files_and_refinement_first() {
        let plan = plan();
        let files = HandoffFiles {
            context: PathBuf::from("/h/jacobragsdale-715/context.md"),
            skill: PathBuf::from("/h/skills/ticket-agent-workflow/SKILL.md"),
        };
        let prompt = opening_prompt(&plan, &checkout(), &files);
        assert!(
            prompt.starts_with("Work item #715 in jacobragsdale/development"),
            "{prompt}"
        );
        assert!(
            prompt.contains("\"Fix `duplicate` imports; $(echo hi)\""),
            "the title travels verbatim: {prompt}"
        );
        assert!(
            prompt.contains("/h/skills/ticket-agent-workflow/SKILL.md"),
            "{prompt}"
        );
        assert!(
            prompt.contains("/h/jacobragsdale-715/context.md"),
            "{prompt}"
        );
        assert!(
            prompt.contains(
                "Do not change any file or start implementing until I tell you to proceed"
            ),
            "{prompt}"
        );
        assert!(prompt.contains("only #715"), "{prompt}");
        assert!(
            prompt.ends_with("Note from me: Keep the public API as it is."),
            "{prompt}"
        );
        assert!(
            prompt.len() < 1200,
            "the prompt stays short; the file carries the rest: {}",
            prompt.len()
        );
    }

    #[test]
    fn the_files_land_under_the_handoff_directory_and_never_in_the_repository() {
        let dir = tempdir().unwrap();
        let plan = plan();
        let files = write(dir.path(), &plan, &checkout()).unwrap();
        assert_eq!(
            files.context,
            dir.path().join("jacobragsdale-715").join("context.md")
        );
        assert_eq!(
            files.skill,
            dir.path().join("skills/ticket-agent-workflow/SKILL.md")
        );
        assert_eq!(std::fs::read_to_string(&files.skill).unwrap(), SKILL);
        assert_eq!(
            std::fs::read_to_string(files.context.with_file_name("prompt.md")).unwrap(),
            opening_prompt(&plan, &checkout(), &files),
            "the prompt on disk is the one sent"
        );
        assert!(SKILL.contains("name: ticket-agent-workflow"));
        assert!(SKILL.contains("Do not start implementing during refinement"));
        assert!(SKILL.contains("prs create"));
        assert!(
            !SKILL.contains("implement it on `main`"),
            "the portable skill carries none of this repository's own conventions"
        );
        // A second write replaces the context in place.
        write(dir.path(), &plan, &checkout()).unwrap();
        assert!(
            std::fs::read_to_string(&files.context)
                .unwrap()
                .contains("#715")
        );
    }

    #[test]
    fn shell_words_are_quoted_only_when_they_have_to_be() {
        assert_eq!(shell_word("payments-api"), "payments-api");
        assert_eq!(shell_word("/a/b.sqlite3"), "/a/b.sqlite3");
        assert_eq!(shell_word("Fi quants"), "'Fi quants'");
        assert_eq!(shell_word("it's"), "'it'\\''s'");
        assert_eq!(shell_word(""), "''");
    }
}
