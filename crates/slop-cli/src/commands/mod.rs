mod artifact;
mod chat;
mod discovery;
mod orchestration;
mod projects;
mod provider;
mod session;
mod turn;

use slop_client::DaemonClient;

use crate::{
    Result,
    args::{Args, Command},
    connection::{ConnectionOptions, authenticated_client},
    output::Output,
};

struct Context {
    connection: ConnectionOptions,
    output: Output,
}

impl Context {
    fn client(&self) -> Result<DaemonClient> {
        authenticated_client(&self.connection)
    }
}

pub async fn run(args: Args) -> Result<()> {
    let Args {
        daemon,
        json,
        token_file,
        command,
    } = args;
    let context = Context {
        connection: ConnectionOptions { daemon, token_file },
        output: Output::new(json),
    };

    match command {
        Command::Task { command } => orchestration::task(command, &context).await,
        Command::Run { command } => orchestration::run(command, &context).await,
        Command::Batch { command } => orchestration::batch(command, &context).await,
        Command::Status => discovery::status(&context).await,
        Command::Node => discovery::node(&context).await,
        Command::Models => discovery::models(&context).await,
        Command::Capabilities => {
            let response = context.client()?.capabilities().await?;
            context.output.value(&response, || {
                for tool in &response.tools {
                    println!("{}\t{}", tool.name, tool.description);
                }
            })
        }
        Command::Artifact { command } => artifact::run(command, &context).await,
        Command::Provider { command } => provider::run(command, &context).await,
        Command::Project { command } => projects::project(command, &context).await,
        Command::Workspace { command } => projects::workspace(command, &context).await,
        Command::Session { command } => session::run(command, &context).await,
        Command::Turn { command } => turn::run(command, &context).await,
        Command::Chat(args) => chat::run(args, &context).await,
    }
}
