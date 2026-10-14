use clap::Subcommand;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum PeerCommand {
    /// List outbound owner registrations (including removed tombstones).
    List,
    /// Register an owner with its private scoped peer credential.
    Add {
        id: String,
        #[arg(long)]
        origin: String,
        #[arg(long)]
        owner_node_id: String,
        #[arg(long)]
        credential_file: PathBuf,
    },
    /// Authenticate and verify a registered owner's stable identity.
    Probe { id: String },
    /// Remove outbound access and close its event streams.
    Remove { id: String },
    /// List this owner's inbound grants.
    Grants,
    /// Grant access to all public conversations on this owner. Control also
    /// authorizes messages that can execute tools under existing session policy.
    Grant {
        #[arg(long)]
        label: String,
        #[arg(long,default_value="read",value_parser=["read","control"])]
        scope: String,
        /// Save the secret privately before issuing the grant; never prints it.
        #[arg(long)]
        token_output: PathBuf,
        /// Stable grant identity, reused for an explicit retry.
        #[arg(long)]
        command_id: String,
    },
    /// Permanently revoke an inbound grant and close its event streams.
    Revoke { id: String },
}
