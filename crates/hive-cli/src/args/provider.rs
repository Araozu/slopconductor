use std::path::PathBuf;

use clap::Subcommand;

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
    /// Authorize a ChatGPT subscription in a browser.
    Login {
        #[arg(value_parser = ["codex"])]
        provider: String,
        #[arg(long)]
        command_id: Option<String>,
        /// Stable registration label; a new label adds another ChatGPT account.
        #[arg(long)]
        account: Option<String>,
        /// Paste the final callback URL on stdin; supports a remote browser
        /// without forwarding the daemon's loopback port.
        #[arg(long)]
        headless: bool,
    },
    /// Inspect a daemon-owned ChatGPT login after detaching.
    LoginStatus { login_id: String },
    /// Probe the selected provider's live catalog without making an inference.
    Discover { provider: String },
    /// List saved ChatGPT registrations and the active account.
    Accounts,
    /// Select an existing Codex billing path without resupplying credentials.
    Select {
        #[arg(value_parser = ["api_key", "chatgpt"])]
        auth_mode: String,
        #[arg(long)]
        account: Option<String>,
    },
    /// Sign out one ChatGPT registration, retaining its client/host identity.
    Logout { account: String },
    /// Remove a saved API key and disable stale environment-key import.
    RemoveKey { provider: String },
}
