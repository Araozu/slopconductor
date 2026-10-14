use clap::Subcommand;

use super::{
    ArtifactCommand, BatchCommand, ChatArgs, ProjectCommand, ProviderCommand, RunCommand,
    SessionCommand, TaskCommand, TurnCommand, WorkspaceCommand,
};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Submit and inspect durable jobs.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Control or explicitly retry a durable attempt.
    Run {
        #[command(subcommand)]
        command: RunCommand,
    },
    /// Preview, submit, and inspect prompt/model/settings matrices.
    Batch {
        #[command(subcommand)]
        command: BatchCommand,
    },
    /// Check daemon connectivity, API compatibility, and capabilities.
    Status,
    /// Configure scoped daemon-to-daemon access.
    Peer {
        #[command(subcommand)]
        command: super::PeerCommand,
    },
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
