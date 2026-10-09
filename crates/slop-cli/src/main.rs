mod args;
mod commands;
mod connection;
mod follow;
mod history;
mod input;
mod mutation;
mod output;

use std::{error::Error, process::ExitCode};

use clap::Parser;

use args::Args;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match commands::run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("slop: {error}");
            ExitCode::FAILURE
        }
    }
}
