use super::*;
use crate::agents::{
    AgentEvent, AgentRequest, AgentSession, AgentSettings, CheckoutPolicy, Provider,
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
fn w_on_a_linked_and_routed_work_item_launches_without_asking() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = agent_app(dir.path());
    let action = press(&mut app, KeyCode::Char('w'));
    let AppAction::Agent(AgentRequest::Launch(plan)) = action else {
        panic!("w launches: {action:?}");
    };
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
        "the spinner turns while it launches"
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
    let AppAction::Agent(AgentRequest::Launch(plan)) = press(&mut app, KeyCode::Enter) else {
        panic!("Enter on a provider launches");
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
    let AppAction::Agent(AgentRequest::Prompt(plan)) = app
        .work_items
        .run_command(&mut app.shell, CommandId::CopyAgentPrompt)
    else {
        panic!("the prompt is copied without Herdr");
    };
    assert_eq!(plan.ticket.id, 715);
    assert_eq!(plan.workspace, "", "no workspace is needed for a copy");
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
    let AppAction::Agent(AgentRequest::Launch(plan)) = press(&mut app, KeyCode::Enter) else {
        panic!("Enter on a workspace launches");
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
    let AppAction::Agent(AgentRequest::Launch(plan)) = action else {
        panic!("Ctrl-S launches too: {action:?}");
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
            .is_some_and(|(text, _)| text.starts_with("Launching")),
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
        AppAction::Agent(AgentRequest::Launch(_))
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
    let AppAction::Agent(AgentRequest::Launch(plan)) = press(&mut app, KeyCode::Char('w')) else {
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
