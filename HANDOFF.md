# Where to pick up

Last updated 2026-09-01. The backlog itself lives in Azure DevOps
(`jacobragsdale/development`); this file is only the pointer into it. Run
`ticket-tui` and browse the epics for the current state.

## State of `main`

- **The ergonomics round is done** (Epic #735, 2026-09-01; Issues #736–#741,
  see "Ergonomics and environment board, 2026-09-01" below): `g` follows the
  link on every tab, `[`/`]` retrace every visit, `+` captures a thought into
  an `inbox` Issue from any tab, `ticket-tui comment ID -` reads its body from
  a pipe, `ticket-tui status` prints the tab badges for a prompt, and
  `[notify]` in `config.toml` raises a desktop notification when a watched run,
  a vote, a review request, a pod or an approval changes.
- **The environment board is done** (Epic #743, 2026-09-01; Issues #744–#749):
  `[deployment]` + `[[environments]]` name a kustomize repository and its
  overlays; `ticket-tui env list|show|check|diff` render them with `kubectl
  kustomize` and read names only; `env check` is the pre-deploy gate (exit 1
  on findings, 2 when an overlay will not render), joined to the environment's
  Key Vault; tab `8` is the board with the promotion diff in its pane; a pull
  request against the deployment repository carries a pre-flight on tab 3.
  **No real deployment repository has been rendered yet** — everything was
  built against `fixtures/kustomize`; see "Environment board — integration
  checklist" below for the day one is cloned.
- **A review + QA round followed both epics** (#750, 2026-09-01): three
  read-only reviewers over `e286f63..main`, every finding fixed or accepted
  with a reason on the ticket, the theme matrix and the ignored real-`kubectl`
  test green, and two pty walks kept beside the AKS one:
  `scripts/walk_ergonomics.py` and `scripts/walk_environments.py`.
- **The Work items tab opens on Mine** when there is no session file yet
  (#753); a remembered session is restored over it.
- Two ideas are written down as `maybe` tickets and not scheduled: #742 (`T`
  dispatches an agent: gather → prompt → launch, one worded verb per tab) and
  #752 (tab 9 Artifacts: Azure Artifacts, PyPI, npm, NuGet feeds).
- **The four-tab roadmap is done**: Epics 656 (#661–#667), 657 (#669–#673),
  658 (#674–#679), 659 (#680–#689) and 660 (#690–#694) are all closed, as is
  #668, and the gate below was green when it closed.
- **A review round followed the roadmap** (evening of 2026-08-29; see "Review
  round" below): the four tabs were read end to end as a QA pass, and what it
  found is fixed and on `main`.
- **The look-and-feel round is done** (Epic #701, Issues #702–#710, planned in
  `LOOK_AND_FEEL_PLAN.md`; see "Look and feel, 2026-08-29" below). The theme
  engine, the table's width rule, the rounded frame with one seam between
  panes, the one-row search, the two-segment status bar, dimmed modals with
  chip buttons, the details pane's badge row and rules, and one spinner for
  every wait are all on `main`.
- **Every tab is drawn by one pane system** (2026-08-29; see "One pane system"
  below): the same two panes, three layouts and draggable seam on all seven.
- **The AKS tab is done** (Epic #718: #719 the worker and the config, #720 the
  table, #721 the log pane, #722 the verbs, #723 the CLI and these docs). Tab
  `5` reads the clusters `config.toml` names through `kubectl` on a worker of
  its own, and `ticket-tui pods` is the same read without the TUI. There is no
  cluster reachable from this machine yet, so every one of those tickets was
  built and tested against `aks::tests::FakeKube` — see "AKS tab — integration
  checklist" below for the day one is.
- **The ACR tab is done** (Epic #724). Tab `6` lists the subscription's
  container registries and drills into one registry's repositories, tags and
  manifests, and `ticket-tui acr` is the same read without the TUI.
- **The Key Vault tab is done** (Epic #725: #730 the tab, #731 the CLI, #732
  the context, the skill and these docs). Tab `7` lists the subscription's
  vaults, drills into one vault's secrets, keys and certificates, and reveals
  one secret's value on `R` for a minute and nowhere else.
- **One `config.toml` now holds the whole setup** (#734, 2026-08-31; see
  "Configuration, 2026-08-31" below): `[devops]` names the organization, the
  project the work items live in and the `code_project` the repositories, pull
  requests and pipelines live in; `[azure]` names the subscriptions and,
  optionally, which registries and vaults out of them are worth listing.
- **The review round over all three new tabs is done** (#733, 2026-08-31; see
  "Review round, 2026-08-31" below): three read-only reviewers over
  `0745f38..main` plus a pty walk-through of every AKS verb against
  `scripts/fake-kubectl` (`scripts/walk_aks.py`). Everything it confirmed is
  fixed on `main`, each with the test that would have caught it.
- **Neither ARM tab has ever been run against a real subscription**: there is
  none reachable from this machine, so `src/arm.rs`, `src/arm_watch.rs`, both
  tabs and all five CLI groups were built against `arm::tests::FakeArm`. See
  "ARM tabs — integration checklist" below for the day one is.
- The gate is `cargo fmt --check`, `cargo clippy --all-targets --all-features
  -D warnings`, `cargo test --all-targets` (817 lib + 34 bin tests, two ignored because they
  shell out to `az` and to `kubectl kustomize` — `cargo test -- --ignored
  kustomize` runs the second) and
  `cargo build --release`, with the test run repeated under `NO_COLOR=1`,
  `TICKET_TUI_THEME=terminal-light` and `TICKET_TUI_THEME=mono` — the theme
  matrix, which is real because `Theme::from_env` reads the variable.
- Database schema is `PRAGMA user_version = 17`. A schema bump drops and
  rebuilds rather than migrating, so the first launch of a build that raises it
  does one full pull automatically — and a running `ticket-tui` binary from
  before the bump refuses to open the file until it is rebuilt.

## The module tree (#661, #662, #698)

`src/app.rs`, `src/ui.rs` and `src/run.rs` are directory modules; every file is
under 1,500 lines. `App` is `{ shell, tab, work_items, repos, pull_requests, pipelines, aks, acr,
key_vault, environments }`:
`Shell` is the state every screen shares and each tab is a `Screen`. A screen
method that needs the shell takes `shell: &mut Shell` (or `&Shell`) as its
first argument after the receiver; nothing else reaches across.

    src/app/mod.rs      App, AppAction, and the event entry points
        screen.rs       the Screen trait every tab implements, and the mouse
                        pipeline it reads for all of them
        shell.rs        focus, the pointer, the notification, the layout, sync
        work_items/     the work items screen
            mod.rs      WorkItemsScreen, WorkItemMode, the key map, the footer
            context.rs  the JSON context file agents read
            edits.rs    edits, undo, bulk, comments, reparenting, deletion
            family.rs   the family tree, its cursor, child progress
            forms.rs    the new-work-item form
            history.rs  bookmarks, the checked set, copy, export, the session
            pickers.rs  state, priority, assignee, parent, node, type pickers
            pointer.rs  what a click on a work-item target does
            query.rs    the search box, filters, facets, columns, commands
            views.rs    saved and built-in views, the sprint summary, staleness
            tests/      the same split, plus tests/deletes.rs
        repos/          the Repos tab: the table, the workspace, the git keys
        pull_requests/  the Pull requests tab: votes, complete, comment
        pipelines/      the Pipelines tab: two levels, the timeline, the log
        aks/            the AKS tab: the pod table, the details and text panes,
                        and the verbs — mod.rs, columns.rs, tests.rs
        acr/            the ACR tab: registries and one registry's catalog —
                        mod.rs, columns.rs, filters.rs, rows.rs, tests.rs
        key_vault/      the Key Vault tab: vaults and one vault's items, and
                        the reveal that clears itself — the same five files
        environments/   tab 8: services × environments, the column cursor,
                        the promotion pane — the same five files
    src/kustomize.rs    the environment model: render (kubectl kustomize, with
                        a deadline), parse (names only, never a value), expand
                        (the `*` glob), check and check_with (the repo-only and
                        vault findings), VaultNames
        kustomize/diff.rs  the promotion diff between two environments and the
                        image read-back to runs, pull requests and work items
    src/preflight.rs    a pull request's pre-flight: the touched overlays
                        rendered at both branches in scratch worktrees, checked
                        and diffed, the worktrees removed however it leaves
    src/notify.rs       [notify]: the command, its quoting, and the three
                        edge diffs (votes, pods, approvals) behind the events
    src/status.rs       `ticket-tui status`: the badges from SQLite and the
                        context file, on one line
    src/watch.rs        the pipeline watcher thread: live runs, timelines, logs,
                        approvals — cadences that stretch, and no SQLite at all
    src/local.rs        the local-repos thread: the workspace scan and the three
                        git commands the Repos tab runs — also no SQLite
    src/aks.rs          the AKS worker thread: Pod, PodRow, PodSchema, the
                        KubeSource trait and the Kubectl behind it, PodWatcher
                        with a 15 s Cadence per cluster, and the one
                        `kubectl logs -f` child — no SQLite either
    src/arm.rs          the Azure Resource Manager client the ACR and Key Vault
                        tabs read through: ArmConfig and how a subscription is
                        resolved, the three token audiences, the Resource Graph
                        inventory query, the two data planes, and Secret —
                        whose Debug and Display both print [redacted]
    src/arm_watch.rs    the ARM worker thread, one for both tabs: the 60 s
                        inventory cadence while either is showing, one read per
                        focus under it, and the reveal that is answered once
                        and never held — no SQLite here either
    src/main.rs         opens the module below and reports what it returns
    src/run/mod.rs      run: the database, the workers, the terminal, and back
        engines.rs      the sync, details, local and reload workers, and the
                        agent context file they publish through
        events.rs       the event loop and the actions it hands back
        dispatch.rs     every request that leaves the run, and its timer
        polling.rs      one non-blocking pass over each worker's channel
        editor.rs       the description round trip through $VISUAL/$EDITOR/vi
        desktop.rs      the clipboard and the browser
        pointer.rs      the mouse pointer shape, and what the hover means
        tests/          the same split, over one fake Azure DevOps — and
                        tests/aks.rs, over one fake kubectl
    src/ui/mod.rs       render, render_screen, the layout, the theme, anchoring
        panes.rs        the pane system every tab is arranged by: the three
                        layouts, the draggable seam, the narrow switcher
        details.rs      the details pane and the family tree it draws
        overlays.rs     the list overlays, chips and facet bar
        pickers.rs      pickers, the prompt, the form, the delete confirmation
        table.rs        the table, its cells and their colours
        widgets.rs      the modal frame, query field, controls, scrollbar, paint
        repos.rs        the Repos tab's own renderer
        pull_requests.rs  the Pull requests tab's own renderer
        pipelines.rs    the Pipelines tab's own renderer
        aks.rs          the AKS tab's own renderer: the table, the pod details
                        and the log pane under them
        acr.rs          the ACR tab's own renderer: the two tables, the
                        registry and repository panes, the tags and manifest
        key_vault.rs    the Key Vault tab's own renderer: the two tables, the
                        vault and item panes, the expiry colours and the one
                        line a revealed value is drawn on
        tests/          the same split, plus tests/acr.rs and
                        tests/key_vault.rs
    scripts/fake-kubectl  a kubectl that answers from fixtures: get pods, a
                        streaming logs -f, describe, delete and exec
    scripts/walk_aks.py walks every AKS verb in the release binary under a pty
                        against that fake, PASS/FAIL per step (needs `pyte`)
    scripts/walk_ergonomics.py  the same shape over the ergonomics round: g,
                        [ ], +, status against the live TUI, the session
    scripts/walk_environments.py  the same over tab 8, with fixtures/kustomize
                        made into a clone and rendered by the real kubectl

Every ui function takes `screen` and `shell` rather than `app`, and
`ui::render` paints the tab bar itself and then goes through `App::screen()`
and `Screen::render` for the rest of the frame.

Shared pieces the later tabs are meant to reuse: `columns::ColumnId` and
`TableLayout<C>` with `ui::table::render_list_table` (#663), `app::ListCursor`
(#663), `filter::FilterSchema` with `FilterSet<S>` and `parse_query::<S>`
(#664), and `PlaceholderScreen` (#665), which is what tabs 2–4 show until
their own tickets land. Pointer targets never name one screen's types:
`SortHeader` carries a column key, `FacetPill` a field key, `RemoveChip` and
`SelectTab` an index.

**The key map changed in #665**: `F` more filters (was `+`), `c` columns (was
`w`), `v` views (was `V`), `V` save view, `e` opens the Actions menu (the Edit
menu renamed), and row density and search order are palette-only. `1`–`4`
switch tabs. Commands carry a `Scope` — `Global` or `Tab(TabId)` — which is
what keeps another tab's keys out of the palette and groups the help.

## Shipped 2026-08-29

Read path (Epic 603, closed): throttling backoff honouring `Retry-After`
(#645). **A pre-existing bug was fixed along the way**: the WIQL endpoint was
called without `timePrecision=true`, so every incremental pull whose watermark
carried a time failed with HTTP 400 — the 60-second background refresh had been
silently failing and only full pulls worked.

Editing (Epic 611, closed): bulk edit over the checked set with one summary
notification (#619), `u` undo — a bulk change undoes as one unit (#646),
`Delete work item…` with a confirm overlay that names the orphaned children
(#647).

Creating (Epic 620, closed): a generic `FormOverlay` behind `n` (#621), `N` for
a child with the type, area and iteration inherited (#622), Set parent /
Remove parent through a cycle-safe picker, one PATCH with `test /rev` (#623).

Agents (Epic 624, closed): context JSON v2 with a `sync` block and
`pending_edits` (#625), CLI subcommands `sync|show|list|edit|comment|create`
(#626), the rewritten `ticket-tui-context` skill (#627). #643, the in-TUI agent
chat pane, was closed on 2026-08-29 as decided against: an agent in its own
terminal reading `tickets.context.json` through the skill and writing through
the CLI covers the need, so the TUI stays a viewer and grows no chat UI.

Manager workflows (Epic 628, closed): built-in views with `@me` / `@none` /
`@current` / `@open` sentinels (#629), sprint summary overlay (#630), stale
highlighting sharing #649's predicate (#631), child progress (#648), `changed:`
and `created:` date filters (#649), finished tickets hidden by default (#652).

The four tabs (Epics 656–660, all closed): the shell split and its shared
scaffolding (#661–#667), repos in the pull (#668), Pipelines with the live log
tail and approvals (#680–#689), Pull requests with votes, complete, abandon,
auto-complete and one-line comments (#674–#679), and Repos with the workspace
scan, clone, fetch, pull and cross-tab jumps (#669–#673). Two background
threads carry the live parts: `watch.rs` for pipelines and `local.rs` for git,
and neither writes SQLite. Then the cross-links and the agent surface
(#690–#694): a work item's artifact links, context schema 3 describing all four
tabs, `repos`/`prs`/`pipelines`/`runs`/`approvals` on the command line with
`runs wait` and `runs logs --follow` exiting on the build's own result, and the
skill rewritten around all of it.

Also: the CLI now resolves query sentinels through the same `MatchContext` the
TUI uses, and refuses one it cannot resolve rather than returning an empty list.
`ticket-tui sync` caches the classification trees, which only opening a picker
used to do.

## Configuration, 2026-08-31 (#734)

`config.toml` is now the one file a machine is set up with, and
`config.example.toml` at the root is a commented copy of the whole of it. Two
tables joined the theme and `[[clusters]]`, both optional and both ignored by
an older build:

- `[devops]` — `org`, `project` (where the work items live), `code_project`
  (where the repositories, pull requests and pipelines live; left out, the
  project), `query`, and `workspace`, whose leading `~/` is the home directory.
- `[azure]` — `subscriptions`, and the optional `registries` and `vaults`
  allowlists, which keep only what they name, in the order they name it,
  without regard to case.

The order for every setting is flag → `TICKET_TUI_*` → `config.toml` → the
Azure CLI (`az devops configure`, `az account show`), settled once in
`Cli::with_file_defaults` straight after `Cli::parse()`, so the TUI and every
subcommand read the same file. A file that will not parse is a startup error
for all of them; the mid-run reload in `run/polling.rs` still reports one in
the footer.

`AzureConfig` gained `code_project` and `AzureClient::code_url`, which is what
every `git`, `build`, `pipelines`, `approvals` and `pullrequests` URL is built
from — the `wit`, `wiql`, teams and work-item URLs keep `project`, and the MSA
pass-through header is unchanged. `ArmConfig` became
`{ subscriptions, registries, vaults }`: one Resource Graph query covers every
subscription at once, `az account show` is asked only when nothing named one,
and the status bar joins the subscriptions with `, `.

The review of that delivery added `SyncEvent::Warning`: a repositories,
pipelines or pull requests read that fails while the pull itself lands is now
said — in the footer, and on stderr from `ticket-tui sync` — so a mistyped
`code_project` is not mistaken for a project with nothing in it. Subscription
ids are trimmed as written, and `config.example.toml` starts on the
`terminal` theme rather than painting the sample palette.

Untouched on purpose: the theme engine, `[[clusters]]`, the process vocabulary,
and `X-VSS-ForceMsaPassThrough`. Still unproven against a real split-project
organization or a multi-subscription tenant — there is neither on this machine,
so both paths are covered by tests only.

## Review round, 2026-08-31

The three new tabs (5 AKS, 6 ACR, 7 Key Vault) were read as one change by
three reviewers — the AKS side, the ARM side, the seams with the rest of the
app — and the AKS tab was driven live under a pty with `scripts/fake-kubectl`.
What they confirmed, all fixed in `27464d6` and the commit after it:

- The restart confirm did not take the pointer: a click on the details pane's
  own `Restart` chip behind the modal deleted the pod, and `c`, `?`, `p`, `i`
  and the digits reached past the confirm (a click through the Columns editor
  could then delete it). The modal covers the whole screen on its own layer,
  and `Screen::modal_open` makes every key answer a confirm first — on the
  Pipelines and Pull requests confirms too.
- A `kubectl get pods` sweep blocked every follow, describe and delete sent
  meanwhile (12 s in the repro): the worker reads one namespace per poll.
- Narrowing a cluster's `namespaces` left the dropped namespace's pods on the
  table for ever; a describe that landed after the cursor moved was shown
  under the new pod and flipped the pane back after `L`; the "earlier lines
  skipped" count was always 1; the log title's spinner froze on a quiet pod;
  a `kubectl logs -f` child could outlive the process on `q`.
- A registry call's 401 never re-minted the ARM token, so the ACR tab died
  after an hour; the refresh token is now exchanged once per registry;
  `Retry-After: nan` panicked the worker; a refusal's `error.details` — where
  Resource Graph puts the reason — is part of its message.
- A reveal nobody answered, or walked away from, spun the tab at 10 Hz for the
  rest of the run; `busy()` now counts only on the tab showing; a value can no
  longer be hung on a key of the same name; `Y` copies only what is shown; a
  disabled certificate's expiry is muted and not counted.
- `r` inside a registry or a vault re-reads it, not only the inventory.
- One unknown jump kind in `history` threw away the whole session file for an
  older build; `g` on a pod records `Jump::Pod`, so `[` comes back; a missing
  ARM jump says "subscription"; `plus_seconds` saturates; a click on a tab's
  search row no longer reaches the work items through a shared overlay; the
  one test that shelled out to `az` is `#[ignore]`.

Left as house behaviour: the Columns overlay's edits to a second-level layout
(repositories, items, runs) are not saved by the session, on every tab that
has a second level.

## Code review pass, 2026-08-29

A cleanup slice ahead of the Repos and Pipelines tabs, with no behaviour change
except one: the deprecated `--sync` flag is gone (`ticket-tui sync --full` is
the replacement, and the wrong-project notification now names it). What went:
the `blocking_sync` path that duplicated the worker's full pull; `SessionKey`
(`TicketKey` derives serde and the file shape is unchanged); the shortcut
fields on `HitRegions` (`table_body`, `headers`, `detail_links`… — every reader
now goes through `find_target`); `FamilySnapshot::tree_entries`/`jump_keys`
and the `other_links` half of the snapshot, which nothing drew; the `UrlOpener`
trait (a `Fn(&Url)`); and a dozen accessors with no callers. What was folded:
`model::same_text` replaces four case-insensitive comparisons, `App::write_refusal`
the six copies of the offline check, `SyncRuntime::send` the five copies of
"hand it to the worker or explain", `stamp_database` the nine
`configure_database` calls, `AzureClient::wit_url` the five hand-built URLs,
`ui::render_query_field` the four picker filter fields, and
`sync::pull_summary` the wording the TUI and the CLI both had.

## The four-tab roadmap (Epics 656–660, created 2026-08-29)

ticket-tui grows into a terminal front-end for the whole project: Work items ·
Repos · Pull requests · Pipelines, keys `1`–`4`, with the browser (`o`) for
anything complicated. The design lives in the epics; every issue carries
Problem / Approach / Done-when, and **Epic 656's description is the working
agreement** for all of them (worker and watcher rules, the shared glyph
language, keyboard–mouse parity, the test gates). Read it before any ticket.

Build order: **656 Tab-ready shell** (661 split files → 662 extract the
screen → 663 list scaffolding → 664 filter schema → 665 tab bar and key map →
666 per-tab session → 667 jumps and `id:`), then **668** repos sync (pulled
forward: pull requests and runs name repos), then **659 Pipelines** (680–689;
the live log tail, 684, is the critical feature of the roadmap), **658 Pull
requests** (674–679), the rest of **657 Repos** (669–673), and **660
Cross-links and agents** (690–694).

Decisions behind the tickets: no terminal hand-off and no Herdr integration
(Repos is detect / clone / status / fetch / pull, not a git client); pull
request actions in the TUI are vote, complete, abandon, auto-complete and a
one-line comment, never the diff; pipelines get trigger, cancel, retry,
approvals, and live status and logs through a second `PipelineWatcher` thread
that never writes SQLite; agents are first-class through `ticket-tui`
subcommands and context v3.

## What is left

Nothing on the four-tab roadmap: Epics 656–660 are all closed. The last slice
(Epic 660) added the artifact links a work item carries, context schema 3, the
`repos`/`prs`/`pipelines`/`runs`/`approvals` subcommands, and the rewritten
`ticket-tui-context` skill in `.agents/skills/`, which is the place to start for
anything an agent should know about this project.

Three things the finished tabs left behind, all noted on tickets:

- The facet bar and chip row are still written against the work items screen,
  so the other three tabs filter through their search box but draw no pills
  (#681).
- The Pipelines details pane does not scroll as a whole: the log has its own
  scroll, the header, Related links and timeline above it do not (#689).
- The Actions overlay is work-items-only. (The command palette, the help, the
  columns editor and the database overlay are not any more: the review round
  below opened them on every tab.)

Four bugs the CLI work turned up in code that had already shipped, all fixed —
worth knowing about because they share a shape: **the fake source in the tests
implemented what the real client did not**.

- `AzureClient` never overrode `pull_request_action`, `comment_on_pull_request`
  or `pull_request_threads`, so completing, abandoning, auto-completing and
  commenting on a pull request all failed against the real API and the
  discussion was always empty (#677, #678).
- A completion never carried `lastMergeSourceCommit`, though the worker's doc
  comment promised it (#677).
- The pull-request list endpoint does not always answer with `_links`, so every
  stored pull request had an empty URL and `o` opened nothing (#674).
- `parse_timeline` sorted by an `order` field that only ranks siblings, and a
  job's log lives on the Phase record that gets flattened away — so the tree
  read out of order and no job had a log (#693).

## Ergonomics and environment board, 2026-09-01 (Epics #735, #743)

Twelve issues, implemented by one Opus agent each in a git worktree — five in
parallel, then three, then the board — with the orchestrator reading every
diff, merging, gating and closing the ticket. The decisions made at the seams
are on the tickets themselves; the ones worth knowing before touching the code:

- `Screen::follow_target` answers `Result<(Jump, &str), String>` — the `Err`
  is the row-specific refusal the footer shows — and only offers what the
  database holds (`shell.work_item_title`, `pull_request_label`, `run_label`).
  `App::new` publishes the titles it is given so tests read the same rows.
- History records on the browser's rule: `App::record_move` around every key
  and every press/release, when the tab or the `Jump` variant changed; a
  follow and the `[`/`]` walk mark `moved` so nothing is recorded twice; a
  target that has gone falls out of the walk.
- `[notify]` values are single-quoted shell words; the first read of each
  source is the baseline, and the pull requests loaded at startup are seeded
  as one (`App::seed_pull_request_marks`) so the first pull can be news.
- `kustomize::parse` keeps names only; a `Secret`'s `data` never reaches a
  type. `check_with` is pure over one `EnvManifest` plus an optional
  `VaultNames`; the CLI reads vaults live and the TUI passes what tab 7 holds.
  `diff` pairs workloads by name and kind, preferring the same namespace, since
  qa and prod keep namespaces of their own.
- The pre-flight is cached per `(pull request, source head)`; `r` re-flies;
  one is in the air at a time; scratch worktrees under `$TMPDIR` are swept on
  the next run even after a kill.

## Environment board — integration checklist

The day a real deployment repository is in the workspace:

1. `config.toml`: `[deployment] repo = "<name as the Repos tab shows it>"`,
   and one `[[environments]]` per overlay set with `overlays` globs relative to
   the clone, `vault`, `registry`, `cluster`. `render = "kustomize build"` if
   the repository needs a newer kustomize than kubectl embeds.
2. `ticket-tui env list` — every environment says `rendered` or the renderer's
   own last line. A remote `resources:` URL in a kustomization will hit the
   120 s render deadline; vendor it or raise `RENDER_TIMEOUT`.
3. `ticket-tui env show prod` — read the providers block: the vault name each
   `SecretProviderClass`/`ExternalSecret` resolves to must be the environment's
   own, or every check for it reads as `pulls from <other>`.
4. `ticket-tui env check` — expect noise first: `envFrom` on a Secret the
   provider fills whole (`dataFrom`) is silent, a volume with `items` is
   checked per key; a projected volume is read. Anything false is a parse gap
   in `src/kustomize.rs` — add the YAML to the tests and fix the reader.
5. `ticket-tui env diff qa prod <service>` — the image read-back needs the
   runs on file (`build_number` or `source_version` prefix) and a clone of the
   service's repository named like the workload; the OCI `revision` rule is a
   parameter nobody passes yet.
6. Tab `8`, then `r`; then a pull request against the repository on tab 3 —
   the pre-flight fetches through `local::remote_git`, so the MSA header
   applies, and the scratch worktrees appear under `$TMPDIR` for the render.

## AKS tab — integration checklist

Nothing in Epic #718 was ever run against a real cluster: there is none
reachable from this machine, so the worker, the tab and `ticket-tui pods` were
all built against `aks::tests::FakeKube`. That is not the same as working. The
day a cluster is reachable, walk this list in order — it is the shortest path
from nothing to a tab that is definitely honest.

**1. Get `kubectl` working outside ticket-tui first.** The TUI shells out to it
and shows what it says; a login problem here reads as a broken tab.

```console
brew install Azure/kubelogin/kubelogin
az aks get-credentials -g <rg> -n <cluster> --subscription <sub>   # per cluster
kubelogin convert-kubeconfig -l azurecli
kubectl --context <ctx> get pods -n <ns>                           # in a plain shell
```

**2. Then the CLI, before the TUI.** Add a `[[clusters]]` table per cluster to
`~/.config/ticket-tui/config.toml` — `name`, `context`, and `namespaces` (leave
it out for `--all-namespaces`) — and run:

```console
ticket-tui pods
ticket-tui pods --json
```

They take the same read path the tab does, with none of the drawing, so a
failure here is the read and not the screen.

**3. Then the tab.** `5`, and work down:

- Both clusters' rows appear as each read lands, not only when the slowest one
  does.
- `/` `cluster:prod status:running` narrows; a header click sorts; `c` puts Node
  and Repo on the table.
- Select a pod: the log tails within a second. Scroll up — the title says
  `scrolled`; `End` — `following`.
- `P` on a CrashLoopBackOff pod shows the run before the last restart. `C` on a
  multi-container pod moves between them. `D` then `L` swaps describe and log.
- `s` opens a shell in the pod, and the TUI repaints when it exits.
- `x` on a qa pod: the confirmation names the owner, a second `x` sends it, the
  row goes `Terminating`, and the replacement appears within about a second.
- `g` jumps to the repository — only for a pod whose image or app label names
  one the project has.
- Edit `config.toml` while it runs: the new cluster is read at once.
- Set one cluster's context to a name that does not exist: its message shows in
  the table status and in the details pane, and the other cluster is unaffected.
- Leave the tab and come back: the stream survives. Quit with `q` and check
  `pgrep kubectl` leaves nothing behind.

## ARM tabs — integration checklist

Same story as AKS: nothing in Epics #724 and #725 was ever run against a real
subscription, so the client, the worker, both tabs and all five CLI groups were
built against `arm::tests::FakeArm`. That is not the same as working. The day a
subscription is reachable, walk this in order.

**1. Get the Azure CLI working outside ticket-tui first.** Everything here
borrows its login, and a login problem reads as two broken tabs.

```console
az login
az account set --subscription <id>          # or pass --subscription / set TICKET_TUI_SUBSCRIPTION
az account show --query id -o tsv
```

An Azure DevOps personal access token does not reach a subscription:
`AZURE_DEVOPS_EXT_PAT` alone leaves both tabs offline, on purpose.

**2. Then the CLI, before the TUI.** Same read path, none of the drawing, so a
failure here is the read and not the screen.

```console
ticket-tui acr list
ticket-tui vaults list
ticket-tui acr repos list --registry <r>
ticket-tui certs list --vault <v>
```

**3. Then tab `6`.**

- Registries land within a minute of opening the tab, and again on `r`.
- `Enter` on one: the catalog appears, the counts fill in behind it, and the
  bottom border counts the attributes reads while they land.
- The details pane's tags fill, and the Manifest section follows the tag cursor
  (`Tab`, then `j`/`k`, or a click).
- `y` copies a pull reference that actually pulls — paste it after `docker pull`.
- `D` copies the digest. `o` opens the registry in the portal.
- `h` or `Backspace` goes back up and the cursor stays where it was.

**4. Then tab `7`.**

- Vaults land; `Enter` opens one and the three kinds arrive as one listing.
- `kind:cert expires:<+30d` narrows to what is running out, and the `◇N` badge
  on the tab agrees with the rows.
- `R` on **one secret you are allowed to see**: the value shows, `clears in 60s`
  counts down, and it is gone after a minute. Leave the tab and come back — it
  is gone. Press `r` — it is gone.
- `Y` while it is showing copies it; `Y` after it has cleared says
  `Nothing is revealed to copy`.
- A secret you are *not* allowed to read: the refusal shows under `Problem` and
  the rest of the listing stays on screen.
- `grep` the context file and the session file for the value you revealed. Both
  must be clean.

**5. Both tabs, wrong on purpose.** Start with `--subscription
00000000-0000-0000-0000-000000000000`: both tabs say so rather than drawing an
empty table with no explanation. Start with no subscription at all and no `az
account`: the same, in `arm.last_error`.

Settled in #733 (`r` now re-reads the open registry or vault too); the note as it stood: `r` re-reads the *inventory* and re-asks for the
current focus, but not the items of a registry or vault that is already open and
already read — the focus reads are once-per-focus by design. Decide there
whether `r` should force those too.

## Known data issue, not a code bug

`development\Sprint 1` has **no start or finish dates** in Azure DevOps, so
`current_iteration()` returns `None`. The `Current sprint` built-in view
therefore lists nothing, and the sprint summary falls back to the selected row's
iteration. Setting the dates on the iteration makes both work.

## Working agreement

Each ticket carries Problem / Approach / Done-when. Flow: set the ticket to
Doing (`ticket-tui edit N --state Doing`), implement it on `main`, keep
fmt/clippy/test/release green, commit and push at each working checkpoint, then
comment on the ticket saying what shipped and set it Done. One ticket at a time,
in the build order; no worktrees and no feature branches.

## Review round, 2026-08-29 (evening)

Jacob reported two bugs off the bat — a clone that sat on `cloning…` for ever,
and "hovering selects the element" — and asked for the whole four-tab delivery
to be reviewed as a QA pass. Everything below is fixed and on `main`.

**The clone.** `C` cloned over ssh, and the machine's key is not registered
with Azure DevOps, so ssh fell back to a *password prompt on /dev/tty* — the
terminal the TUI owns — and git waited for ever. `local.rs` now runs every
remote git command with prompts off (`GIT_TERMINAL_PROMPT=0`, `ssh -o
BatchMode=yes -o ConnectTimeout=20`, stdin closed), a 30-second stall timeout
(`http.lowSpeedLimit/Time`), and — the part that makes it *work* rather than
merely fail fast — signs an Azure DevOps https remote with the same
`authorization_header()` the sync uses, plus `X-VSS-ForceMsaPassThrough: true`,
which git needs for an MSA-backed org exactly as the REST calls do. The header
is scoped to the host (`http.https://dev.azure.com.extraheader`) and passed as
`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` so it is never offered to GitHub and
never shows in `ps`. https is the default clone URL now;
`TICKET_TUI_CLONE_PROTOCOL=ssh` asks for ssh. Fetch and pull of a clone whose
origin is an Azure DevOps https remote are signed the same way.

**The hover.** Not reproduced: driving the release binary under a pty with
Ghostty-style SGR motion (`ESC [ < 35 ; x ; y M`) hovers rows (the
`Indexed(237)` tint from #642) and never moves the selection, and grok-night's
palette makes the selection (`#6c6c6c`) brighter than the tint. One real defect
was found on the way and fixed: a click on the tab bar recorded its press on
the screen but the release was swallowed by `App::handle_mouse`, so the pointer
believed the button was still down and every later move read as a drag. If
the report stands after that, the next thing to capture is the raw byte stream
Ghostty sends while hovering.

**Everything else the pass found**, each with a test:

- The Repos tab was empty until the first pull (`App::relate_repos` now runs at
  start-up too).
- `?`, `p`/`:`, `c` and `i` did nothing on tabs 2–4 while their footers said
  `? help`: the four shared overlays now open on every tab, drawn and driven
  by the work items screen on the other tab's behalf (`App::overlay_for`,
  `AppAction::RunCommand`); the palette lists that tab's commands and the
  columns editor edits that tab's layout.
- The Pull requests and Pipelines verbs were raw keys with no command behind
  them, so the help listed none of them and the `[Approve]`… and
  `[Cancel] [Retry]` buttons had no click regions. They are scoped commands
  now (`CommandId::ApprovePr`… `WatchRun`), and the buttons are the keys.
- The Repos, Pull request and run details panes placed their click targets by
  line index while wrapping long lines, so once a URL or title wrapped every
  target under it was off by a row or more (`ui::widgets::wrapped_rows`). The
  Repos and Pull request panes scroll now, with a `Details` scroll region.
- A timer pull replaced the pull request and run lists and kept only the row
  *number*, so the hand moved to a different row when something new arrived;
  the watcher's `merge_live_runs` did the same. Both keep the selection by id,
  and `set_pipelines` keeps what the watcher already saw (a run newer than the
  pull's window, or one it has already seen finish).
- A jump to a pull request, pipeline, run or repository the tab's query hid
  said "not in this database"; the query is cleared instead.
- `Enter` on the runs level re-opened the runs and reset the cursor; `R`
  retried runs that had succeeded; `W` on a finished run produced an instant
  "finished" toast rather than a refusal.
- A pull request was only re-read when its head or reviewers moved, so the
  Build column never left `queued`; one whose policy build is undecided is
  read again (`sync::build_undecided`).
- `runs trigger --follow` failed at once ("has written no log yet") because a
  just-queued run has no timeline, and `--follow` stopped when the first node
  finished; it waits, and follows the run node by node to the end.
- `runs trigger` sent `refs/heads/refs/heads/<branch>` (`start_run` takes
  either spelling now); `prs vote` by a non-reviewer left the stored copy
  without the vote.
- The help popup wrapped its longest rows; the empty table said "No tickets
  in this database" when all 100 were merely finished and hidden.


## Look and feel, 2026-08-29 (Epic #701)

Nine issues, each its own commit, in the order the plan set. What each changed
and why is in its ADO comment; the shape of it:

- **#702 theme engine.** `config.toml` in `$XDG_CONFIG_HOME/ticket-tui/`, the
  `terminal` / `terminal-light` / `mono` / `custom` presets, `--theme`,
  `TICKET_TUI_THEME`, the palette the `theme` tool writes, and live reload.
- **#706 table layout.** A table drops its right-most optional column while the
  flexible one would fall under `min_flexible_width` (24 for a title, 16 for a
  repository name, 20 for a pipeline). The scrollbar has its own column, so a
  cell is never painted over. State cells carry the family tree's glyph, Pri
  reads `P1`. `TableLayout::auto_hide` is gone: nothing turned it back on.
- **#703 frame.** The theme's corners, `border`/`border_focused`, and one
  border column shared between neighbouring panes (`Spacing::Overlap(1)` and
  `MergeStrategy::Fuzzy` — `Exact` cannot merge rounded corners). A list
  pane's counts moved to its bottom border, except where that border is the
  seam. The details panes are padded and their scrollbars have their own
  column.
- **#704 search row.** One row, not a box three rows tall, on every tab;
  `Actions` and `?` are the tab bar's, and the shell answers them.
- **#705 status bar.** `Shell::sync_status()` replaced `activity_label()`; the
  footer is hints on the left and the sync state on the right, on every tab.
- **#707 overlays.** Dimmed behind, sized by share of the screen, chip buttons
  with the primary action filled, an accent `›` and a muted key label on list
  rows.
- **#708 details.** A badge row instead of two prose lines, section rules,
  an aligned label column, a fractional progress bar.
- **#709 motion.** One braille spinner for every wait, turning only while
  something is in flight, and a flash on the row an edit lands on.
- **#710 docs.** This file, the README screenshot, DESIGN.md, and the theme
  matrix in the gate.

Not done, deliberately: Nerd Font icons, images, `tachyonfx`, and any change to
the keys or the hit-region model. `pane_split_wide` is still 62 by default; a
session that has dragged the divider keeps what it dragged, and `Reset pane
split` in the palette puts it back.

## One pane system, 2026-08-29

The work items tab could be resized by dragging the border between its panes;
the other three drew a fixed split — 55/45, 58/42, 62/38 — with no seam, no
stacked layout, and no details pane at all below 110 columns. Now there is one
way to draw and manage a pane, and all four tabs go through it.

- `src/ui/panes.rs` owns the arrangement. `render_workspace` picks the layout
  from the width — side by side from 110, stacked from 70, one pane below that
  — registers the seam, paints both halves and draws the seam over the border
  they share. A tab passes a `PanePair` (its list and its details) and what to
  call them; two closures could not, because both halves want the screen.
- `render_inner_split` is the same seam inside one pane, for the pipelines log
  under its run. It follows the pane's shape rather than the terminal's: it
  stacks in a tall pane, sits side by side in a short wide one, and stands down
  where there is room for neither, leaving `l` to show the log whole.
- `PointerTarget::PaneDivider` carries a `PaneSplit` (`Workspace` or
  `Details`), and `Shell` keeps a `PaneSeam` per split — orientation, the
  workspace it divides, and the cells each side keeps — registered every frame
  by the renderer. `drag_divider(split, ..)` reads it and writes the matching
  percentage, so one drag serves every seam. The third percentage,
  `pane_split_details`, is in the session file with the other two.
- The mouse pipeline moved from `WorkItemsScreen` into the `Screen` trait, so
  press, drag and release mean the same thing on every tab: seams drag, text
  selects, scrollbar thumbs work, and a click acts on what was pressed rather
  than on whatever the pointer has wandered onto.
- The narrow switcher is the pane system's: it repaints the top border and
  writes ` [Repos] [Repository] ` chips over it, on whichever pane is showing.
  It used to be the tickets table's title, which meant the details pane below
  70 columns had no way back to the list with the mouse.
- `Toggle details pane` (`d`) and `Reset pane split` are global commands now,
  answered by all four screens, and `Tab` goes through `Shell::toggle_focus` /
  `focus_list` everywhere: below 70 columns it used to move the focus to a
  details pane that was not on screen.

The tabs' fixed splits are gone: every tab opens at 62/56 and keeps what it was
last dragged to, on whichever tab dragged it.
