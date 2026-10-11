use clap::Parser;
use std::path::PathBuf;

use super::*;

#[test]
fn managed_workspaces_require_an_unambiguous_target() {
    let args = Args::try_parse_from([
        "slop",
        "chat",
        "--project",
        "project-1",
        "--base",
        "main",
        "--tool",
        "edit",
        "--prompt",
        "fix",
    ])
    .unwrap();
    assert!(
        matches!(args.command, Command::Chat(ChatArgs { project: Some(project), base: Some(base), .. }) if project == "project-1" && base == "main")
    );
    for args in [
        vec!["slop", "chat", "--project", "p", "--workspace", "/tmp"],
        vec!["slop", "chat", "--project", "p", "--session", "s"],
        vec!["slop", "chat", "--base", "main"],
        vec!["slop", "chat", "--tool", "edit"],
    ] {
        assert!(Args::try_parse_from(args).is_err());
    }
}

#[test]
fn model_selection_is_preserved_when_resuming_a_session() {
    let parsed = Args::try_parse_from([
        "slop",
        "chat",
        "--session",
        "session-1",
        "--model",
        "opencode-go/glm-5.3-flash",
        "--prompt",
        "hello",
    ]);
    let args = parsed.unwrap();
    let Command::Chat(chat) = args.command else {
        panic!("expected chat")
    };
    assert_eq!(chat.model.as_deref(), Some("opencode-go/glm-5.3-flash"));
}

#[test]
fn execution_delivery_and_pause_commands_parse() {
    let session = Args::try_parse_from([
        "slop",
        "session",
        "send",
        "session-1",
        "--text",
        "hello",
        "--delivery",
        "immediate",
    ])
    .unwrap();
    assert!(
        matches!(session.command, Command::Session { command: SessionCommand::Send { delivery: Some(mode), .. } } if mode == "immediate")
    );

    let chat = Args::try_parse_from([
        "slop",
        "chat",
        "--session",
        "session-1",
        "--prompt",
        "hello",
        "--delivery",
        "next-boundary",
    ])
    .unwrap();
    assert!(
        matches!(chat.command, Command::Chat(ChatArgs { delivery: Some(mode), .. }) if mode == "next-boundary")
    );
    assert!(
        Args::try_parse_from([
            "slop",
            "chat",
            "--prompt",
            "hello",
            "--delivery",
            "immediate"
        ])
        .is_err()
    );

    let pause = Args::try_parse_from(["slop", "turn", "pause", "turn-1"]).unwrap();
    assert!(
        matches!(pause.command, Command::Turn { command: TurnCommand::Pause { id, .. } } if id == "turn-1")
    );
}

#[test]
fn prompt_sources_are_mutually_exclusive() {
    for command in [
        vec![
            "slop",
            "chat",
            "--prompt",
            "hello",
            "--prompt-file",
            "prompt.txt",
        ],
        vec![
            "slop",
            "session",
            "send",
            "session-1",
            "--text",
            "hello",
            "--prompt-file",
            "prompt.txt",
        ],
    ] {
        assert!(Args::try_parse_from(command).is_err());
    }
}

#[test]
fn global_options_work_before_and_after_nested_commands() {
    for command in [
        vec![
            "slop",
            "--daemon",
            "http://127.0.0.1:7441",
            "--json",
            "--token-file",
            "token",
            "session",
            "show",
            "session-1",
        ],
        vec![
            "slop",
            "session",
            "show",
            "session-1",
            "--daemon",
            "http://127.0.0.1:7441",
            "--json",
            "--token-file",
            "token",
        ],
    ] {
        let args = Args::try_parse_from(command).unwrap();
        assert_eq!(args.daemon, "http://127.0.0.1:7441");
        assert!(args.json);
        assert_eq!(args.token_file, Some(PathBuf::from("token")));
        assert!(matches!(
            args.command,
            Command::Session { command: SessionCommand::Show { id } } if id == "session-1"
        ));
    }
}
