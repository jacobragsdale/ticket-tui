use super::*;
use crate::agents::{
    AgentEvent, AgentRequest, AgentSession, AgentSettings, CheckoutPolicy, LaunchPlan, Provider,
};
use crate::config;
use crate::model::{ArtifactKind, ArtifactLink, PrStatus};

fn key(id: i64) -> TicketKey {
    TicketKey {
        organization: "demo".into(),
        id,
    }
}

/// An app inside Herdr with two repositories on file, #715 linked to a
/// branch of the first, #716 linked to nothing, and a file that routes the
/// first repository to Payments.
fn agent_app(dir: &std::path::Path) -> App {
    let mut app = App::new(vec![
        ticket(715, "Fix the thing!", "2026-01-05T00:00:00Z"),
        ticket(716, "Unlinked", "2026-01-04T00:00:00Z"),
    ]);
    app.shell.set_repos(vec![
        crate::app::repos::tests::repo("aaa-111", "payments-api", false),
        crate::app::repos::tests::repo("bbb-222", "reporting-api", false),
    ]);
    app.shell.set_sync_target(Some(SyncTarget {
        organization: "demo".into(),
        project: "atlas".into(),
        code_project: "code".into(),
        teams: Vec::new(),
        refresh_seconds: 60,
    }));
    app.shell.configure_database(dir.join("tickets.sqlite3"), 0);
    app.shell
        .set_launch_environment(true, dir.join("config.toml"));
    app.shell.set_agent_settings(AgentSettings::from_config(
        &config::parse(
            "[herdr]\n[[herdr.workspaces]]\nname = \"Payments\"\nrepos = [\"payments-api\"]\n",
        )
        .unwrap(),
    ));
    let graph = TicketGraph {
        artifacts: vec![ArtifactLink {
            work_item: key(715),
            kind: ArtifactKind::Branch {
                repo_id: "aaa-111".into(),
                name: "715-fix-the-thing".into(),
            },
            name: "Branch".into(),
        }],
        ..TicketGraph::default()
    };
    app.work_items.set_workspace_graph(&mut app.shell, graph);
    app.work_items.select_row(&mut app.shell, 0);
    app
}

/// The thread's answer to a Prepare: the prompt editor opens on `prompt`.
fn prepared(app: &mut App, plan: Box<LaunchPlan>, prompt: &str, copy_only: bool) {
    app.work_items.apply_agent_event(
        &mut app.shell,
        AgentEvent::Prepared {
            plan,
            prompt: prompt.into(),
            generated: prompt.into(),
            copy_only,
        },
    );
}

fn ctrl(app: &mut App, ch: char) -> AppAction {
    app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL))
}

fn editor_text(app: &App) -> &str {
    app.work_items.handoff.as_ref().unwrap().input.text()
}

pub(crate) fn live_session(id: i64) -> AgentSession {
    AgentSession {
        id: format!("demo-{id}-1"),
        organization: "demo".into(),
        project: "atlas".into(),
        work_item: id,
        repo_id: "aaa-111".into(),
        repo_name: "payments-api".into(),
        workdir: "/src/.worktrees/payments-api/715-fix-the-thing".into(),
        branch: "715-fix-the-thing".into(),
        policy: CheckoutPolicy::Worktree,
        provider: Provider::Copilot,
        workspace: "Payments".into(),
        workspace_id: Some("w1".into()),
        tab_id: Some("w1:t2".into()),
        pane_id: Some("w1:p3".into()),
        agent_name: Some(format!("wi-{id}")),
        context_path: None,
        prompt: Some("go".into()),
        prompt_sent: true,
        started_at: "2026-09-06T00:00:00Z".into(),
    }
}

#[test]
fn w_on_a_linked_and_routed_work_item_prepares_without_asking_and_the_editor_launches() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = agent_app(dir.path());
    let action = press(&mut app, KeyCode::Char('w'));
    let AppAction::Agent(AgentRequest::Prepare { plan, copy_only }) = action else {
        panic!("w prepares the prompt: {action:?}");
    };
    assert!(!copy_only);
    assert_eq!(plan.ticket.id, 715);
    assert_eq!(plan.ticket.organization, "demo");
    assert_eq!(plan.ticket.title, "Fix the thing!");
    assert_eq!(plan.repo.name, "payments-api");
    assert_eq!(plan.workspace, "Payments");
    assert_eq!(plan.provider, Provider::Cursor, "the default provider");
    assert_eq!(
        plan.policy,
        CheckoutPolicy::Worktree,
        "worktrees by default"
    );
    assert_eq!(plan.linked_branch.as_deref(), Some("715-fix-the-thing"));
    assert_eq!(plan.branch(), "715-fix-the-thing");
    assert_eq!(plan.invocation.organization, "demo");
    assert_eq!(plan.invocation.project, "atlas");
    assert_eq!(plan.invocation.code_project, "code");
    assert_eq!(plan.invocation.database, dir.path().join("tickets.sqlite3"));
    assert_eq!(plan.handoff_dir, dir.path().join("handoffs"));
    assert!(!plan.force_new);
    assert_eq!(
        plan.ticket.links,
        ["Branch 715-fix-the-thing in payments-api"]
    );
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert!(
        app.work_items.agent_busy().is_some(),
        "the spinner turns while the prompt is prepared"
    );
    assert_eq!(
        press(&mut app, KeyCode::Char('w')),
        AppAction::None,
        "one launch at a time"
    );
    assert!(
        app.shell
            .notification()
            .is_some_and(|(text, _)| text.starts_with("Wait for the agent thread"))
    );

    // The thread answers with the prompt: it opens for editing, and nothing
    // is launched until Ctrl-S.
    prepared(
        &mut app,
        plan,
        "/ticket-agent-workflow\nWork item #715.",
        false,
    );
    assert_eq!(app.work_items.mode, WorkItemMode::Handoff);
    assert!(app.work_items.agent_busy().is_none());
    let editor = app.work_items.handoff.as_ref().unwrap();
    assert_eq!(
        editor.input.text(),
        "/ticket-agent-workflow\nWork item #715."
    );
    assert!(!editor.is_dirty());
    assert_eq!(
        editor.title(),
        " Prompt for Cursor on #715 \u{00b7} payments-api "
    );
    press(&mut app, KeyCode::Enter);
    for ch in "Keep the API.".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(
        app.work_items.handoff.as_ref().unwrap().title(),
        " Prompt for Cursor on #715 \u{00b7} payments-api \u{00b7} edited "
    );
    let AppAction::Agent(AgentRequest::Launch(plan)) = ctrl(&mut app, 's') else {
        panic!("Ctrl-S launches");
    };
    assert_eq!(
        plan.prompt.as_deref(),
        Some("/ticket-agent-workflow\nWork item #715.\nKeep the API."),
        "what was typed is what goes"
    );
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("Launching Cursor on #715 in Payments\u{2026}")
    );
    assert!(app.work_items.agent_busy().is_some());

    // The thread answers: the session is on file and the status says so.
    let mut session = live_session(715);
    session.context_path = Some(dir.path().join("handoffs/demo-715/context.md"));
    app.work_items
        .apply_agent_event(&mut app.shell, AgentEvent::Sessions(vec![session.clone()]));
    app.work_items.apply_agent_event(
        &mut app.shell,
        AgentEvent::Launched {
            session: Box::new(session.clone()),
            note: "made tab payments-api".into(),
        },
    );
    assert!(app.work_items.agent_busy().is_none());
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("Copilot is on #715 in Payments \u{203a} payments-api \u{2014} made tab payments-api")
    );
    assert_eq!(app.work_items.live_agent_for(&key(715)), Some(&session));

    // Now `w` goes back to it rather than launching another.
    assert_eq!(
        press(&mut app, KeyCode::Char('w')),
        AppAction::Agent(AgentRequest::Return("demo-715-1".into()))
    );
    // And the explicit verb starts another, asking which provider.
    assert_eq!(
        app.work_items
            .run_command(&mut app.shell, CommandId::StartAnotherAgentSession),
        AppAction::None
    );
    assert_eq!(app.work_items.mode, WorkItemMode::AgentPicker);
    assert_eq!(app.work_items.agent_picker.choice, AgentChoice::Provider);
    assert_eq!(app.work_items.agent_matches(), ["copilot", "cursor"]);
    press(&mut app, KeyCode::Down);
    let AppAction::Agent(AgentRequest::Prepare { plan, .. }) = press(&mut app, KeyCode::Enter)
    else {
        panic!("Enter on a provider prepares the prompt");
    };
    assert_eq!(plan.provider, Provider::Cursor);
    assert!(plan.force_new);
}

#[test]
fn outside_herdr_w_refuses_and_the_prompt_can_still_be_copied() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = agent_app(dir.path());
    app.shell
        .set_launch_environment(false, dir.path().join("config.toml"));
    assert_eq!(press(&mut app, KeyCode::Char('w')), AppAction::None);
    assert!(
        app.shell
            .notification()
            .is_some_and(|(text, _)| text.starts_with("Not inside Herdr")),
        "{:?}",
        app.shell.notification()
    );
    let AppAction::Agent(AgentRequest::Prepare { plan, copy_only }) = app
        .work_items
        .run_command(&mut app.shell, CommandId::CopyAgentPrompt)
    else {
        panic!("the prompt is prepared without Herdr");
    };
    assert!(copy_only);
    assert_eq!(plan.ticket.id, 715);
    assert_eq!(plan.workspace, "", "no workspace is needed for a copy");
    // The same editor, with Copy on its button; Ctrl-S copies the text.
    prepared(&mut app, plan, "the prompt", true);
    assert_eq!(
        app.work_items.handoff.as_ref().unwrap().title(),
        " Prompt to copy for #715 \u{00b7} payments-api "
    );
    let AppAction::Agent(AgentRequest::Prompt(plan)) = ctrl(&mut app, 's') else {
        panic!("Ctrl-S copies");
    };
    assert_eq!(plan.prompt.as_deref(), Some("the prompt"));
    assert!(app.work_items.agent_busy().is_some());
    let copied = app.work_items.apply_agent_event(
        &mut app.shell,
        AgentEvent::Prompt {
            work_item: 715,
            text: "the prompt".into(),
            context: dir.path().join("context.md"),
        },
    );
    assert_eq!(copied.as_deref(), Some("the prompt"));
    assert!(app.work_items.agent_busy().is_none());
    assert!(
        app.work_items.handoff_drafts.is_empty(),
        "a copy that landed keeps no draft"
    );
}

#[test]
fn an_unlinked_work_item_asks_for_the_repository_then_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = agent_app(dir.path());
    app.work_items.select_row(&mut app.shell, 1);
    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 716);
    assert_eq!(press(&mut app, KeyCode::Char('w')), AppAction::None);
    assert_eq!(app.work_items.mode, WorkItemMode::AgentPicker);
    assert_eq!(app.work_items.agent_picker.choice, AgentChoice::Repo);
    assert_eq!(
        app.work_items.agent_matches(),
        ["payments-api", "reporting-api"]
    );
    for ch in "rep".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(app.work_items.agent_matches(), ["reporting-api"]);
    // reporting-api is routed nowhere: the workspace is asked, and Herdr is
    // asked for its live workspaces meanwhile.
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        AppAction::Agent(AgentRequest::Workspaces)
    );
    assert_eq!(app.work_items.agent_picker.choice, AgentChoice::Workspace);
    assert_eq!(
        app.work_items.agent_matches(),
        ["Payments"],
        "the configured ones first"
    );
    app.work_items.apply_agent_event(
        &mut app.shell,
        AgentEvent::Workspaces(vec!["payments".into(), "Reporting".into()]),
    );
    assert_eq!(
        app.work_items.agent_matches(),
        ["Payments", "Reporting"],
        "the live ones join, without a duplicate of one already listed"
    );
    // A name none of them has is offered as a new workspace.
    for ch in "Data".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(app.work_items.agent_matches(), ["Data"]);
    let AppAction::Agent(AgentRequest::Prepare { plan, .. }) = press(&mut app, KeyCode::Enter)
    else {
        panic!("Enter on a workspace prepares the prompt");
    };
    assert_eq!(plan.repo.name, "reporting-api");
    assert_eq!(plan.workspace, "Data");
    assert_eq!(plan.linked_branch, None);
    assert_eq!(plan.branch(), "716-unlinked", "a branch from the title");
    assert!(
        !dir.path().join("config.toml").exists(),
        "Enter alone remembers nothing"
    );
}

#[test]
fn ctrl_s_on_a_workspace_remembers_the_routing_in_config_toml() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "# mine\n[devops]\norg = \"demo\"\n").unwrap();
    let mut app = agent_app(dir.path());
    app.work_items.select_row(&mut app.shell, 1);
    press(&mut app, KeyCode::Char('w'));
    for ch in "reporting".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.work_items.agent_picker.choice, AgentChoice::Workspace);
    for ch in "Reporting".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    let AppAction::Agent(AgentRequest::Prepare { plan, .. }) = action else {
        panic!("Ctrl-S prepares the prompt too: {action:?}");
    };
    assert_eq!(plan.workspace, "Reporting");
    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        written.starts_with("# mine\n[devops]\norg = \"demo\"\n"),
        "{written}"
    );
    assert!(
        written.contains("[[herdr.workspaces]]\nname = \"Reporting\"\nrepos = [\"reporting-api\"]"),
        "{written}"
    );
    assert_eq!(
        app.shell
            .agent_settings()
            .herdr
            .workspace_for("reporting-api"),
        Some("Reporting"),
        "and the running app knows it at once"
    );
    assert!(
        app.shell
            .notification()
            .is_some_and(|(text, _)| text.starts_with("Preparing the prompt")),
    );
}

#[test]
fn w_says_it_will_clone_a_repository_that_is_not_here_and_waits_for_one_being_cloned() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = agent_app(dir.path());
    let root = dir.path().join("dev");
    app.shell.set_workspace(Some(root.clone()));
    assert!(matches!(
        press(&mut app, KeyCode::Char('w')),
        AppAction::Agent(AgentRequest::Prepare { .. })
    ));
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some(
            format!(
                "Cloning payments-api into {}, then preparing the prompt for #715\u{2026}",
                root.display()
            )
            .as_str()
        )
    );
    let free = |app: &mut App| {
        app.work_items.apply_agent_event(
            &mut app.shell,
            AgentEvent::Failed {
                work_item: 715,
                stage: crate::agents::Stage::Checkout,
                message: "no".into(),
                session: None,
            },
        );
    };
    free(&mut app);

    // While the Repos tab is cloning it, w waits rather than racing it.
    app.repos
        .set_job("aaa-111", Some(crate::model::GitJob::Cloning));
    assert_eq!(press(&mut app, KeyCode::Char('w')), AppAction::None);
    assert!(app.work_items.agent_busy().is_none(), "free to try again");
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some(
            "#715: settling the checkout failed: payments-api is cloning on the Repos tab; wait for it"
        )
    );

    // A verified clone here is not cloned again.
    app.repos.set_job("aaa-111", None);
    app.shell
        .set_clones(std::iter::once("aaa-111".to_owned()).collect());
    assert!(matches!(
        press(&mut app, KeyCode::Char('w')),
        AppAction::Agent(AgentRequest::Prepare { .. })
    ));
    assert!(
        app.shell
            .notification()
            .is_some_and(|(text, _)| text.starts_with("Preparing the prompt for #715")),
    );
}

#[test]
fn a_failed_stage_and_a_stale_agent_are_said_and_the_flow_is_free_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = agent_app(dir.path());
    press(&mut app, KeyCode::Char('w'));
    let mut partial = live_session(715);
    partial.agent_name = None;
    partial.prompt_sent = false;
    app.work_items
        .apply_agent_event(&mut app.shell, AgentEvent::Sessions(vec![partial.clone()]));
    app.work_items.apply_agent_event(
        &mut app.shell,
        AgentEvent::Failed {
            work_item: 715,
            stage: crate::agents::Stage::Agent,
            message: "herdr: agent_not_ready: copilot never came up".into(),
            session: Some(Box::new(partial)),
        },
    );
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some(
            "#715: starting the agent failed: herdr: agent_not_ready: copilot never came up \u{2014} w carries on from there"
        )
    );
    assert!(app.work_items.agent_busy().is_none());
    assert_eq!(
        app.work_items.live_agent_for(&key(715)),
        None,
        "an unfinished session is not a live agent"
    );
    // So `w` launches again rather than returning.
    assert!(matches!(
        press(&mut app, KeyCode::Char('w')),
        AppAction::Agent(AgentRequest::Prepare { .. })
    ));

    app.work_items.apply_agent_event(
        &mut app.shell,
        AgentEvent::Stale {
            work_item: 715,
            reason: "no agent is in pane w1:p3 any more".into(),
        },
    );
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some(
            "The agent on #715 is gone: no agent is in pane w1:p3 any more \u{2014} w starts another"
        )
    );
}

#[test]
fn esc_keeps_the_edited_prompt_as_a_draft_and_ctrl_r_brings_the_generated_one_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = agent_app(dir.path());
    let AppAction::Agent(AgentRequest::Prepare { plan, .. }) = press(&mut app, KeyCode::Char('w'))
    else {
        panic!("w prepares");
    };
    // The help opened while the thread worked closes when the prompt lands.
    press(&mut app, KeyCode::Char('?'));
    assert_eq!(app.work_items.mode, WorkItemMode::Help);
    prepared(&mut app, plan, "generated", false);
    assert_eq!(app.work_items.mode, WorkItemMode::Handoff);
    for ch in " plus".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    app.handle_paste("\nand a\r\npaste");
    assert_eq!(editor_text(&app), "generated plus\nand a\npaste");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert!(app.work_items.handoff.is_none());
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("Prompt draft kept on #715 \u{2014} w brings it back")
    );

    // The next w prepares again; the thread's answer opens on the draft.
    let AppAction::Agent(AgentRequest::Prepare { plan, .. }) = press(&mut app, KeyCode::Char('w'))
    else {
        panic!("w prepares again");
    };
    prepared(&mut app, plan, "generated", false);
    assert_eq!(editor_text(&app), "generated plus\nand a\npaste");
    assert!(app.work_items.handoff.as_ref().unwrap().is_dirty());

    // Ctrl-R puts the generated prompt back, caret at the top; Esc on it
    // keeps no draft.
    ctrl(&mut app, 'r');
    let editor = app.work_items.handoff.as_ref().unwrap();
    assert_eq!(editor.input.text(), "generated");
    assert_eq!(editor.input.cursor(), 0);
    assert!(!editor.is_dirty());
    press(&mut app, KeyCode::Esc);
    assert!(app.work_items.handoff_drafts.is_empty());
    let AppAction::Agent(AgentRequest::Prepare { plan, .. }) = press(&mut app, KeyCode::Char('w'))
    else {
        panic!("w prepares a third time");
    };
    prepared(&mut app, plan, "generated again", false);
    assert_eq!(editor_text(&app), "generated again");
}

#[test]
fn an_empty_prompt_is_refused_and_a_failed_launch_gives_the_edit_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = agent_app(dir.path());
    let AppAction::Agent(AgentRequest::Prepare { plan, .. }) = press(&mut app, KeyCode::Char('w'))
    else {
        panic!("w prepares");
    };
    prepared(&mut app, plan, "generated", false);
    ctrl(&mut app, 'u');
    assert_eq!(ctrl(&mut app, 's'), AppAction::None);
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Handoff,
        "the editor stays open"
    );
    assert!(
        app.shell
            .notification()
            .is_some_and(|(text, _)| text.starts_with("The prompt is empty")),
    );
    for ch in "mine".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    let AppAction::Agent(AgentRequest::Launch(plan)) = ctrl(&mut app, 's') else {
        panic!("Ctrl-S launches");
    };
    assert_eq!(plan.prompt.as_deref(), Some("mine"));

    // Herdr refuses the start: the edit comes back on the next w.
    app.work_items.apply_agent_event(
        &mut app.shell,
        AgentEvent::Failed {
            work_item: 715,
            stage: crate::agents::Stage::Agent,
            message: "no".into(),
            session: None,
        },
    );
    let AppAction::Agent(AgentRequest::Prepare { plan, .. }) = press(&mut app, KeyCode::Char('w'))
    else {
        panic!("w prepares again");
    };
    prepared(&mut app, plan, "generated", false);
    assert_eq!(editor_text(&app), "mine");

    // A launch that lands drops it.
    assert!(matches!(
        ctrl(&mut app, 's'),
        AppAction::Agent(AgentRequest::Launch(_))
    ));
    app.work_items.apply_agent_event(
        &mut app.shell,
        AgentEvent::Launched {
            session: Box::new(live_session(715)),
            note: "ok".into(),
        },
    );
    assert!(app.work_items.handoff_drafts.is_empty());
}

#[test]
fn the_plan_carries_the_open_pull_request_of_the_repository() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = agent_app(dir.path());
    let graph = TicketGraph {
        artifacts: vec![
            ArtifactLink {
                work_item: key(715),
                kind: ArtifactKind::PullRequest {
                    repo_id: "aaa-111".into(),
                    id: 42,
                },
                name: "Pull Request".into(),
            },
            ArtifactLink {
                work_item: key(715),
                kind: ArtifactKind::PullRequest {
                    repo_id: "aaa-111".into(),
                    id: 41,
                },
                name: "Pull Request".into(),
            },
        ],
        ..TicketGraph::default()
    };
    app.work_items.set_workspace_graph(&mut app.shell, graph);
    app.shell.set_artifact_labels(
        vec![
            (42, "Fix it".into(), PrStatus::Active),
            (41, "Old try".into(), PrStatus::Abandoned),
        ],
        Vec::new(),
    );
    app.shell.set_pull_request_sources(vec![
        (42, "refs/heads/715-fix".into(), "https://x/42".into()),
        (41, "refs/heads/old".into(), "https://x/41".into()),
    ]);
    let AppAction::Agent(AgentRequest::Prepare { plan, .. }) = press(&mut app, KeyCode::Char('w'))
    else {
        panic!("a pull request pins the repository too");
    };
    assert_eq!(plan.repo.name, "payments-api");
    assert_eq!(
        plan.pull_request,
        Some((42, "715-fix".into(), "https://x/42".into())),
        "the open one, not the abandoned one"
    );
    assert_eq!(
        plan.branch(),
        "715-fix",
        "and its branch is the one to work on"
    );
    assert_eq!(
        plan.ticket.links,
        [
            "Pull request !42 in payments-api (active): Fix it",
            "Pull request !41 in payments-api (abandoned): Old try"
        ]
    );
}

#[test]
fn show_agent_prompt_opens_the_stored_prompt_and_says_when_there_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = agent_app(dir.path());
    assert_eq!(
        app.work_items
            .run_command(&mut app.shell, CommandId::ShowAgentPrompt),
        AppAction::None
    );
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert!(
        app.shell
            .notification()
            .is_some_and(|(text, _)| text.starts_with("No agent prompt on file for #715")),
        "{:?}",
        app.shell.notification()
    );
    // A launch that stopped after its handoff still has the prompt it wrote.
    let mut partial = live_session(715);
    partial.agent_name = None;
    partial.prompt_sent = false;
    app.work_items
        .apply_agent_event(&mut app.shell, AgentEvent::Sessions(vec![partial]));
    app.work_items
        .run_command(&mut app.shell, CommandId::ShowAgentPrompt);
    assert_eq!(app.work_items.mode, WorkItemMode::AgentPrompt);
    assert_eq!(
        app.work_items
            .agent_prompt_for(&key(715))
            .and_then(|session| session.prompt.as_deref()),
        Some("go")
    );
    assert_eq!(press(&mut app, KeyCode::Esc), AppAction::None);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
}
