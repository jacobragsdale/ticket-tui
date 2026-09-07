---
name: ticket-agent-workflow
description: Work one Azure DevOps work item from refinement to a linked draft pull request with ticket-tui. Use when a handoff names a work item and a checkout and asks you to refine it with the user, implement it once told to, verify, push, open a draft PR linked to the ticket, and leave it awaiting human review.
---

# Working a ticket with ticket-tui

You have been handed one Azure DevOps work item and one checkout. The handoff
context file names both, and names the exact `ticket-tui` invocation that
reaches the right organization, board project, code project and local
database. The work item in the handoff is the one you act on: a running TUI
publishes a live context file that follows whatever row the user has selected,
and that selection is not your task.

The flow has two halves. First you refine the ticket with the user, in
conversation; nothing is implemented until they say to proceed. Then you
implement, verify, push a branch, open a draft pull request linked to the
ticket, and stop. Merging, closing and moving the ticket to done are the
user's, not yours.

## 1. Read before you act

- Read the handoff context file end to end. It carries the ticket's identity,
  URL, revision, title, description, acceptance criteria, its parent and
  children, its linked branches and pull requests, and the checkout you are in.
- Read the ticket again, live, so you start from the current revision:

  ```console
  ticket-tui <global flags from the handoff> show <ID>
  ```

  Every `ticket-tui` command below takes the same global flags, written either
  side of the subcommand: `--database`, `--org`, `--project`, `--code-project`.
  The handoff spells them out; copy them rather than guessing.
- Read this repository's own agent instructions — `AGENTS.md`, `CLAUDE.md`,
  `.github/copilot-instructions.md`, `.cursor/rules`, a `CONTRIBUTING.md` —
  and follow them. They say how this repository builds, tests and branches;
  this skill does not.
- Inspect the code the ticket touches before forming an opinion.

## 2. Refine with the user

Discuss what is unclear or undecided: the problem as stated, the scope, the
acceptance criteria, anything the description assumes that the code does not
bear out. Propose changes to the ticket's wording as concrete text. Ask about
decisions rather than making them silently. Keep the conversation in the
terminal you are in; the user answers there.

Do not start implementing during refinement, even for something that looks
trivial. The user says when.

## 3. Save the agreed ticket before implementing

Once the description or acceptance criteria are agreed, write them to the
ticket so the record matches the plan:

```console
ticket-tui <flags> edit <ID> --description-file /tmp/<ID>-description.md
ticket-tui <flags> edit <ID> --acceptance-criteria-file /tmp/<ID>-criteria.md
ticket-tui <flags> edit <ID> --state Doing
ticket-tui <flags> comment <ID> "Refined with the user: <one line on what changed>"
```

Write Markdown to the files; it is converted for Azure DevOps. Every write
leads with the revision the local database holds, so one that lands on a
ticket somebody else has moved is refused rather than overwriting them.

## 4. Revision conflicts: refresh and reconcile

A refused write says `run \`ticket-tui sync\` and try again`. Do exactly that,
re-read the ticket with `show`, look at what changed, fold the other person's
change into what you were about to write, and retry. Never re-send your old
text over theirs. A comment bumps the revision too, so comment first and set
the state after, or sync between the two.

## 5. Implement only when told

When the user says to proceed, work on the branch the handoff names, in the
checkout the handoff names. Do not switch the checkout to another branch, do
not reset, clean or discard changes that were there before you, do not delete
branches, and do not remove worktrees. If the handoff says the checkout is
shared with the user, stay on the branch it names and ask before anything that
would move it.

Commit as the repository's conventions ask. Keep the ticket id in commit
messages where the repository does that.

## 6. Verify the way this repository verifies

Run whatever the repository's own instructions call its gate — its formatter,
linter, tests and build. Report what you ran and what it said, including a
failure; do not describe a check you did not run.

## 7. Push the branch and open a linked draft pull request

Push the implementation branch, then create the pull request with ticket-tui,
which links it to the work item and records it locally so the user's TUI shows
it at once:

```console
git push -u origin <branch>
ticket-tui <flags> prs create \
  --repo <repository name> \
  --source <branch> \
  --target <default branch> \
  --title "<what changed, in one line>" \
  --description-file /tmp/<ID>-pr.md \
  --work-item <ID> \
  --draft
```

`prs create` requires at least one `--work-item`, takes several to link
several tickets, and is safe to run again: a second run finds the active pull
request for the same repository, source and target, leaves its title,
description and draft state alone, and repairs any ticket link that is
missing. It prints the pull request's id and URL, and exits non-zero while a
requested link is still not confirmed — say so rather than reporting success.

Do not merge, complete, or turn on auto-complete.

## 8. Comment a summary on the ticket

Leave one concise comment on the work item: what was implemented, what was
verified and how, the pull request's id and URL, and anything left out or
worth a reviewer's eye:

```console
ticket-tui <flags> comment <ID> -   # body on standard input, as a code block
ticket-tui <flags> comment <ID> "Implemented in !<PR>: <summary>. Verified: <gate>."
```

## 9. Leave it awaiting review

Stop there. The pull request is a draft for a person to review; the ticket
stays in the state the user chose. Do not mark the work item done, do not mark
the pull request ready, and do not merge because a pull request exists. Tell
the user where things stand and what needs their decision next.

## Notes

- This workflow is a conversational agreement, not a permission system. It
  asks you to refine before implementing; it does not stop you. Honour it.
- Secrets never belong in the ticket, the pull request or a comment.
- If `ticket-tui` is not on the path, the handoff says how it is run on this
  machine.
