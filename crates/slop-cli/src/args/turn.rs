use clap::Subcommand;

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
