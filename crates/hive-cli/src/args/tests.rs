use clap::Parser;
use std::path::PathBuf;

use super::*;

#[test]
fn chat_workspace_selection_requires_an_unambiguous_target() {
    let args = Args::try_parse_from([
        "hivectl",
        "chat",
        "--workspace-id",
        "workspace-1",
        "--tool",
        "edit",
        "--prompt",
        "fix",
    ])
    .unwrap();
    assert!(
        matches!(args.command, Command::Chat(ChatArgs { workspace_id: Some(id), .. }) if id == "workspace-1")
    );
    for args in [
        vec![
            "hivectl",
            "chat",
            "--workspace-id",
            "p",
            "--workspace",
            "/tmp",
        ],
        vec!["hivectl", "chat", "--workspace-id", "p", "--no-workspace"],
        vec!["hivectl", "chat", "--workspace-id", "p", "--session", "s"],
        vec!["hivectl", "project", "register", "/tmp"],
        vec!["hivectl", "chat", "--tool", "edit"],
    ] {
        assert!(Args::try_parse_from(args).is_err());
    }
    let args =
        Args::try_parse_from(["hivectl", "chat", "--no-workspace", "--prompt", "text"]).unwrap();
    assert!(matches!(
        args.command,
        Command::Chat(ChatArgs {
            no_workspace: true,
            ..
        })
    ));
}

#[test]
fn model_selection_is_preserved_when_resuming_a_session() {
    let parsed = Args::try_parse_from([
        "hivectl",
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
        "hivectl",
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
        "hivectl",
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
            "hivectl",
            "chat",
            "--prompt",
            "hello",
            "--delivery",
            "immediate"
        ])
        .is_err()
    );

    let pause = Args::try_parse_from(["hivectl", "turn", "pause", "turn-1"]).unwrap();
    assert!(
        matches!(pause.command, Command::Turn { command: TurnCommand::Pause { id, .. } } if id == "turn-1")
    );
}

#[test]
fn prompt_sources_are_mutually_exclusive() {
    for command in [
        vec![
            "hivectl",
            "chat",
            "--prompt",
            "hello",
            "--prompt-file",
            "prompt.txt",
        ],
        vec![
            "hivectl",
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
            "hivectl",
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
            "hivectl",
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
