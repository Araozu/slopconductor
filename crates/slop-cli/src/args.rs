use std::path::PathBuf;

use clap::{Parser, Subcommand};
use slop_protocol::DEFAULT_DAEMON_URL;

#[derive(Debug, Parser)]
#[command(name = "slop", version, about = "Slop Conductor command-line client")]
pub struct Args {
    /// Daemon origin to connect to.
    #[arg(long, global = true, env = "SLOP_DAEMON_URL", default_value = DEFAULT_DAEMON_URL)]
    pub daemon: String,

    /// Emit machine-readable JSON on standard output.
    #[arg(long, global = true)]
    pub json: bool,

    /// Read the local API bearer token from this file.
    #[arg(long, global = true)]
    pub token_file: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check daemon connectivity, API compatibility, and capabilities.
    Status,
    /// Show this daemon's authenticated node identity.
    Node,
    /// Discover available models.
    Models,
    /// Show structured execution and registered tool capabilities.
    Capabilities,
    /// Register and inspect repositories on the daemon's machine.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Inspect or deliberately remove a managed session worktree.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// Inspect or download durable tool output.
    Artifact {
        #[command(subcommand)]
        command: ArtifactCommand,
    },
    /// Configure daemon-owned provider credentials.
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    /// Manage conversations.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Inspect or cancel an execution turn.
    Turn {
        #[command(subcommand)]
        command: TurnCommand,
    },
    /// Start or resume a text conversation.
    Chat(ChatArgs),
}

#[derive(Debug, Subcommand)]
pub enum ProviderCommand {
    /// Show credential presence and daemon execution support, never secrets.
    Status,
    /// Read an API key from stdin (or --key-file) and save it in the daemon.
    SetKey {
        provider: String,
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
    /// Authorize a ChatGPT subscription through the daemon's callback listener.
    Login {
        #[arg(value_parser = ["codex"])]
        provider: String,
        #[arg(long)]
        command_id: Option<String>,
    },
    /// Inspect a daemon-owned ChatGPT login after detaching.
    LoginStatus { login_id: String },
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    List {
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Show {
        id: String,
    },
    History {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Send {
        id: String,
        #[arg(long, conflicts_with = "text")]
        prompt_file: Option<PathBuf>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        command_id: Option<String>,
        #[arg(long)]
        detach: bool,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        max_output_tokens: Option<u32>,
        #[arg(long, value_parser=["after-turn", "next-boundary", "immediate"])]
        delivery: Option<String>,
    },
    Follow {
        id: String,
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
}

#[derive(Debug, Subcommand)]
pub enum TurnCommand {
    Requests {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    Tools {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    Show {
        id: String,
    },
    Cancel {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
    },
    Pause {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
    },
    Resume {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ArtifactCommand {
    Show {
        id: String,
    },
    Download {
        id: String,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    Register {
        /// Absolute repository checkout path on the daemon's machine.
        path: PathBuf,
        #[arg(long)]
        command_id: Option<String>,
    },
    List {
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    Show {
        id: String,
    },
    Workspaces {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    Events {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    Show {
        id: String,
    },
    Diff {
        id: String,
    },
    Remove {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
    },
}

#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("workspace_selection").args(["workspace", "project"]).multiple(false)))]
pub struct ChatArgs {
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Explicit workspace on the daemon's machine for a new tool-enabled session.
    #[arg(long, conflicts_with = "session")]
    pub workspace: Option<PathBuf>,
    /// Registered project on the daemon; reserve an isolated managed worktree.
    #[arg(long, conflicts_with = "session")]
    pub project: Option<String>,
    /// Git revision to freeze when reserving a project worktree (default HEAD).
    #[arg(long, requires = "project")]
    pub base: Option<String>,
    /// Tool to allow; repeat to restrict the default read/write/edit/bash set.
    #[arg(long="tool", requires="workspace_selection", value_parser=["read","write","edit","bash"])]
    pub tools: Vec<String>,
    #[arg(long)]
    pub effort: Option<String>,
    #[arg(long)]
    pub max_output_tokens: Option<u32>,
    #[arg(long, value_parser=["after-turn", "next-boundary", "immediate"], requires="session")]
    pub delivery: Option<String>,
    #[arg(long, conflicts_with = "prompt_file")]
    pub prompt: Option<String>,
    #[arg(long)]
    pub prompt_file: Option<PathBuf>,
    #[arg(long)]
    pub command_id: Option<String>,
    /// Return after durable acceptance without following the turn.
    #[arg(long)]
    pub detach: bool,
}

#[cfg(test)]
mod tests {
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
}
