# Plan: the prompt modal before an agent launch

## Goal

`w` (and **Copy agent prompt**) stop sending anything blind. Before the prompt
reaches the agent it opens in a modal on the work items tab, exactly as it will
be sent, with the workflow skill invoked by a slash command on its first line:

```
/ticket-agent-workflow
Work item #715 in jacobragsdale/development (Azure DevOps): "Fix duplicate imports".
Repository payments-api, checked out at ~/dev/.worktrees/payments-api/715-fix-duplicate-imports on branch 715-fix-duplicate-imports (a git worktree for this ticket alone).
Read these two files before anything else:
1. ~/.local/share/ticket-tui/handoffs/skills/ticket-agent-workflow/SKILL.md — the workflow to follow
2. ~/.local/share/ticket-tui/handoffs/jacobragsdale-715/context.md — the ticket, its context, and the exact ticket-tui invocation
Then, as the skill says: … Do not change any file or start implementing until I tell you to proceed. …
```

The user edits any of it in place, then `Ctrl-S` launches (or copies). What
was sent is what the modal showed, byte for byte, and it is what `prompt.md`
on disk and **Show agent prompt** hold afterwards. Installing the skill under
that slash-command name in the agent is the user's side; the path on line 1
stays as the fallback for an agent without it.

## What the user sees

```
 ┌ Prompt for Cursor on #715 · payments-api · edited ──────────────── [x] ┐
 │ /ticket-agent-workflow                                               ▲ │
 │ Work item #715 in jacobragsdale/development (Azure DevOps): "Fix     █ │
 │ duplicate imports".                                                  █ │
 │ Repository payments-api, checked out at /home/jacob/dev/.worktrees/  │ │
 │ …                                                                    │ │
 │ Keep the public API as it is.▏                                       ▼ │
 │                                                                        │
 │  Launch   Close                                                        │
 └────────────────────────────────────────────────────────────────────────┘
  Enter newline  Ctrl-S launch  Ctrl-R regenerate  Esc keep draft  Ctrl-U clear
```

1. `w` settles the repository, the Herdr workspace and the provider exactly as
   today (pickers when it cannot). The status line turns: *Preparing the
   prompt for #715…* (or *Cloning payments-api into ~/dev, then preparing the
   prompt for #715…*).
2. The modal opens on the prompt as it will be sent. The caret is at the end,
   so a first keystroke appends a note; `Ctrl-Home`-style jumps are `Home` /
   `End`. The box is the same 70 % share of the screen the help and the prompt
   viewer take; a long prompt scrolls, the wheel and the scrollbar work, a
   click puts the caret where it lands, a drag selects and copies, a paste
   keeps its line breaks.
3. `Ctrl-S` (or the **Launch** button) sends. The modal closes, the status
   says *Launching Cursor on #715 in Payments…*, and the launch runs its Herdr
   stages as today. The title reads `· edited` while the text differs from
   what ticket-tui generated. **Copy agent prompt** opens the same modal with
   a **Copy** button; `Ctrl-S` puts the edited text on the clipboard.
4. `Esc` (or **Close**, or the `[x]`) keeps an edited prompt as a draft for
   this run — *Prompt draft kept on #715 — w brings it back* — and the next
   `w` on the same work item and repository opens on the draft. `Ctrl-R` puts
   the generated prompt back at any time. An empty prompt is refused with the
   modal left open.
5. **Show agent prompt** is unchanged: what the agent was actually sent.

The footer hint carries the keys, like every other mode. Nothing else on the
tab changes.

## Decisions and why

**The prompt is built on the agent thread, before the modal opens.** The
prompt names the checkout — the worktree path, the branch, the policy — and
those are settled by git, not by the row on screen. Guessing them on the main
thread and hoping the launch agrees would put a wrong path in front of the
agent the one time it mattered. So a launch becomes two requests to the agent
thread: `Prepare` (checkout settled, handoff written, prompt answered) and,
after the user's `Ctrl-S`, `Launch` with the edited prompt in the plan. The
existing stage-by-stage resume takes it from there unchanged.

**Prepare makes no worktree.** `Esc` must leave nothing behind. `checkout::
settle` gets a `make` flag: with it off, it finds the worktree already
holding the branch (the clone included) or names the path it *would* add,
and adds nothing; the launch runs the same function with `make` on and lands
on the same path, because it is the same decision. A repository that is not
here is still cloned by Prepare — the user asked for a launch, the clone is
what `C` on the Repos tab would make, and predicting a clone that does not
exist yet would be a second code path to keep in step. The status line says
so before it happens.

**One override field, one place that reads it.** `LaunchPlan.prompt:
Option<String>` is the edited text; `handoff::prompt_for(plan, checkout,
files)` answers it or the generated prompt. `handoff::write` writes that to
`prompt.md`, `launch` stores it on the session and sends it, `prompt_only`
returns it. Nothing else knows the difference.

**The slash command is a constant beside the skill.** `handoff::SKILL_COMMAND
= "/ticket-agent-workflow"` is the skill's own `name:`, and a test holds the
two together. No config knob: the modal is where a different first line is
typed, and a knob for a line that is editable on screen is a second way to do
one thing.

**Drafts live in memory, keyed by (work item, repository).** A ticket handed
to two repositories has two prompts with two sets of paths. The draft is kept
when the modal closes without sending *and* while a send is in flight, and is
dropped only when the thread reports the launch landed or the prompt was
copied. So a launch that fails at any stage — the clone, Herdr, the agent's
trust dialog — gives the same edited text back on the next `w`, not the
generated one. Across a restart of ticket-tui, an unsent prompt stored on a
half-made session (`<db>.agents.json`) is offered back the same way.

**One modal for launch and copy.** Same editor, same keys, same draft; only
the primary button's word and the request it sends differ. Two modals would
be two things to keep in step.

**The prompt viewer's plumbing is reused.** The modal scrolls through the
help's `ScrollState` (as **Show agent prompt** already does), its rows are
`ComposerRow` targets (the composer's click-to-caret), its text is the
`TextInput` the composer types into, wrapped by `wrap_with_cursor`. New
pointer plumbing is one `TextEditor::Handoff` (paste routing) and one
`PointerTarget::SendHandoff` (the button).

## Resiliency

| Where it can fail | What happens |
|---|---|
| Prepare: no clone and nothing to clone into, a name-only directory, a path override that is not a clone | The same refusal the launch gives today, at once, before the modal — nothing to edit yet. |
| Prepare: the clone takes a minute | Status says *Cloning … then preparing the prompt*; the UI stays live; a second `w` is refused with *Wait for the agent thread*. |
| The Repos tab is cloning the same repository | `App::apply` turns Prepare into a Checkout failure with *wait for it*, as it does Launch and Prompt. |
| The row changes, a picker or the composer is open when Prepared arrives | The modal is about the plan it carries, not the selection; whatever overlay is open is closed the way `Esc` closes it (drafts kept) and the modal opens. |
| The user presses `Esc` after editing | Draft kept for this run; said in the status line. |
| Launch fails at Checkout (a linked branch nowhere) | Said as today; the draft survives; `w` reopens the modal on it. |
| Launch fails at a Herdr stage | Session stored as today; the draft survives in memory; the pending session carries the edited prompt on disk; `w` reopens the modal on the edited text and the resumed launch skips the stages already made. |
| The prompt is emptied | `Ctrl-S` is refused: *The prompt is empty; Ctrl-R brings the generated one back*. |
| `prompt.md` would disagree with what was sent | It cannot: `write` and the send read the same `prompt_for`. |
| The agent thread has stopped | `Prepare` is never sent; the existing *restart ticket-tui* message. |

## Performance and responsiveness

- **Nothing on the main thread waits on git or the disk.** Prepare runs where
  the launch runs. Its cost without a clone is `git worktree prune`, `git
  worktree list` and three small file writes: tens of milliseconds, under the
  status spinner, on a machine where a worktree add would take the same.
- **The launch after `Ctrl-S` does the checkout for real** — the same calls
  plus `git worktree add` — so the two-phase flow costs one extra listing,
  not a second worktree.
- **Rendering is a wrap of a two-kilobyte string per frame**, the same work
  the composer does for a comment; the rows are drawn pre-wrapped, so no
  second wrap runs inside the widget.
- **PageUp/PageDown move the caret by a screenful** rather than scrolling the
  viewport away from it, so the next keystroke never yanks the view back.
- **The draft map is keyed and small**; nothing is written to disk for it.

## Implementation

Four commits, each green on `cargo fmt --check`, `clippy -D warnings`,
`cargo test --all-targets`, pushed to `main` as they land.

### 1. The prompt: slash command first, one override, one reader

- `src/agents/handoff.rs`: `pub const SKILL_COMMAND: &str = "/ticket-agent-workflow"`;
  `opening_prompt` puts it on its own first line; `pub fn prompt_for(plan,
  checkout, files) -> String` (`plan.prompt` or `opening_prompt`); `write`
  writes `prompt_for` to `prompt.md`.
- `src/agents/mod.rs`: `LaunchPlan.prompt: Option<String>`; `launch` sets
  `session.prompt = Some(prompt_for(…))`; `prompt_only` returns `prompt_for`.
- `src/cli.rs`: the plan literal gains `prompt: None`.
- Tests: the first line is the command and the skill's `name:` matches it;
  `prompt_for` honours the override; `write` puts the override in
  `prompt.md`; the fake Herdr receives exactly the override.

### 2. Prepare on the agent thread, a checkout that names without making

- `src/agents/checkout.rs`: `settle(…, make: bool)`; with `make` off the
  add-a-worktree branch returns `Checkout { workdir: path, note: "worktree to
  be added" }` after the same `exists()` refusal, and neither fetches nor
  creates directories.
- `src/agents/mod.rs`: `enum Purpose { Launch, Prepare, Copy }` replaces the
  `for_launch` flag of `settle_checkout` (Prepare skips the `is_dir` check);
  `pub fn prepare(store, plan, copy_only) -> Result<(String, String),
  LaunchFailure>` — checkout, `handoff::write`, `generated =
  opening_prompt`, `prompt` = the pending session's unsent prompt else
  `generated`; `AgentRequest::Prepare { plan, copy_only }`;
  `AgentEvent::Prepared { plan, prompt, generated, copy_only }`; the thread
  arm; `run/polling.rs` resets the workspace scan on Prepared too (it may
  have cloned).
- Tests: prepare names the worktree path without making it and a launch
  after it lands on that path; a pending unsent prompt is offered back and
  `generated` still is the generated one; copy makes no worktree (as today).

### 3. The modal

- `src/pointer.rs`: `TextEditor::Handoff`, `PointerTarget::SendHandoff`.
- `src/app/work_items/mod.rs`: `WorkItemMode::Handoff`; fields `handoff:
  Option<HandoffEditor>`, `handoff_drafts: HashMap<(TicketKey, String),
  String>`; dispatch, footer hint, `mode_name`, `close_overlay`.
- `src/app/work_items/agent.rs`: `HandoffEditor { plan, copy_only, input,
  generated, width, follow_cursor }` with `is_dirty`, `title`, `hint`;
  `continue_agent_flow` sends `Prepare` and the *Preparing…* status;
  `apply_agent_event` on `Prepared` closes whatever is open, opens the
  editor on the draft / offered prompt, resets the help scroll;
  `handle_handoff_key` (Esc keep, Ctrl-S send, Ctrl-R regenerate, Enter
  newline, Up/Down rows, PageUp/PageDown a screenful, Tab swallowed, the rest
  to `TextInput`); `send_handoff` (empty refused; `plan.prompt = Some(text)`;
  draft kept; status; `Launch` or `Prompt`); `close_handoff`; drafts dropped
  on `Launched` / `Prompt`.
- `src/app/work_items/query.rs`: `active_editor` and `handle_paste` arms.
- `src/app/work_items/pointer.rs`: `ComposerRow` places the caret in
  whichever editor is open; `SendHandoff`; the editor-to-surface map.
- `src/app/mod.rs`: the clone-race guard covers `Prepare`.
- `src/ui/overlays.rs`: `render_handoff_editor` — frame and title with the
  edited marker, pre-wrapped rows at the inner width less the scrollbar
  column, caret via `set_cursor_position`, `ensure_visible` when the editor
  asked to follow, row hit regions on the modal layer, scrollbar, selectable
  capture, the button row (`Launch`/`Copy` primary, `Close`), the close
  glyph. `src/ui/mod.rs` dispatches the mode.
- Tests (`app/work_items/tests/agent.rs`): `w` sends Prepare and is busy;
  Prepared opens the modal on the text; typing then `Ctrl-S` sends `Launch`
  with the edited prompt; `Esc` keeps the draft and the next Prepared opens
  on it; `Ctrl-R` regenerates; an empty prompt is refused; the copy path
  sends `Prompt` with the override and the clipboard gets it; a failed launch
  keeps the draft; Launched drops it. The existing tests that expected
  `Launch` straight from `w` go through the modal with one helper.
  (`ui/tests/overlays.rs`): the modal paints the title, the first line, the
  buttons and the caret; a click on a row moves the caret; the wheel scrolls.

### 4. Docs and memory

- `DESIGN.md` "The flow behind `w`": the prompt step between the handoff and
  Herdr, the draft rule, `Prepare` in the resume paragraph, the slash command
  in "The workflow skill".
- `README.md` `w` row; `command.rs` help for **Work with agent** and **Copy
  agent prompt**.
- The memory note for this round.

## Out of scope, on purpose

- `ticket-tui agent launch --prompt-file`: the plan field makes it a
  six-line follow-up; nobody asked for it yet.
- A config knob for the slash-command name: the line is editable on screen.
- Drafts across restarts: an unsent prompt on a half-made session already
  survives one; an `Esc`-kept draft does not, and a file for it would be a
  third place a prompt lives.
- Syntax colouring or a preview of `context.md` inside the modal: the prompt
  names the file; a second pane can open it.
