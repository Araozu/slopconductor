use super::Context;
use crate::{
    Result,
    args::WorkspaceCommand,
    mutation::{choose_command_id, mutation_error},
};
use hive_protocol::workspaces::*;

pub(super) async fn run(command: WorkspaceCommand, context: &Context) -> Result<()> {
    let client = context.client()?;
    match command {
        WorkspaceCommand::Open { path, command_id } => {
            if !path.is_absolute() {
                return Err("workspace path must be absolute on the daemon's machine".into());
            }
            let command_id = choose_command_id(command_id)?;
            let response = client
                .open_workspace(&OpenWorkspaceRequest {
                    command_id: command_id.clone(),
                    path: path.to_str().ok_or("workspace path must be UTF-8")?.into(),
                })
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context.output.value(&response, || {
                println!(
                    "{}\t{}\t{}",
                    response.name,
                    response.id,
                    response.path.as_deref().unwrap_or("-")
                )
            })
        }
        WorkspaceCommand::Create {
            parent,
            name,
            git,
            command_id,
        } => {
            if !parent.is_absolute() {
                return Err("workspace parent must be absolute on the daemon's machine".into());
            }
            let command_id = choose_command_id(command_id)?;
            let response = client
                .create_workspace(&CreateWorkspaceRequest {
                    command_id: command_id.clone(),
                    parent: parent
                        .to_str()
                        .ok_or("workspace parent must be UTF-8")?
                        .into(),
                    name,
                    git,
                })
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context.output.value(&response, || {
                println!(
                    "{}\t{}\t{}",
                    response.name,
                    response.id,
                    response.path.as_deref().unwrap_or("-")
                )
            })
        }
        WorkspaceCommand::List { after, limit } => {
            let response = client.workspaces(after, Some(limit)).await?;
            context.output.value(&response, || {
                for workspace in &response.items {
                    println!(
                        "{}\t{}\t{}\t{}",
                        workspace.id,
                        workspace.color,
                        workspace.status,
                        workspace.path.as_deref().unwrap_or("-")
                    );
                }
            })
        }
        WorkspaceCommand::Show { id } => {
            let response = client.workspace(&id).await?;
            context.output.value(&response, || {
                println!(
                    "{}\t{}\nName: {}\nColor: {}\nOwner: {}\nPath: {}\nManaged worktree: {}",
                    response.id,
                    response.status,
                    response.name,
                    response.color,
                    response.owner_node_id,
                    response.path.as_deref().unwrap_or("-"),
                    response.managed_worktree
                );
                if response.is_default {
                    println!("Built-in directory-less workspace");
                }
                if let Some(source) = &response.source_workspace_id {
                    println!("Source workspace: {source}");
                }
                if let Some(base) = &response.base_commit {
                    println!("Base: {base}");
                }
                if let Some(error) = &response.error_code {
                    println!(
                        "Error: {error}\nEffects unknown: {}",
                        response.effects_unknown
                    );
                }
            })
        }
        WorkspaceCommand::Worktree {
            id,
            base,
            name,
            command_id,
        } => {
            let command_id = choose_command_id(command_id)?;
            let response = client
                .create_worktree(
                    &id,
                    &CreateWorktreeRequest {
                        command_id: command_id.clone(),
                        base_ref: base,
                        name,
                    },
                )
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context.output.value(&response, || {
                println!(
                    "{}\t{}\t{}",
                    response.id,
                    response.status,
                    response.path.as_deref().unwrap_or("-")
                )
            })
        }
        WorkspaceCommand::Diff { id } => {
            let response = client.workspace_diff(&id).await?;
            context.output.value(&response, || {
                println!(
                    "Workspace: {}\nBase: {}\nHEAD: {}",
                    response.workspace_id, response.base_commit, response.head_commit
                );
                print!("{}{}", response.status, response.patch);
            })
        }
        WorkspaceCommand::Events { id, after, limit } => {
            let response = client.workspace_events(&id, after, Some(limit)).await?;
            context.output.value(&response, || {
                for event in &response.items {
                    println!("{}\t{}\t{}", event.sequence, event.kind, event.workspace_id);
                }
            })
        }
        WorkspaceCommand::Rename {
            id,
            name,
            command_id,
        } => {
            let command_id = choose_command_id(command_id)?;
            let response = client
                .rename_workspace(
                    &id,
                    &RenameWorkspaceRequest {
                        command_id: command_id.clone(),
                        name,
                    },
                )
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context
                .output
                .value(&response, || println!("{}\t{}", response.name, response.id))
        }
        WorkspaceCommand::Color {
            id,
            color,
            command_id,
        } => {
            let command_id = choose_command_id(command_id)?;
            let response = client
                .set_workspace_color(
                    &id,
                    &SetWorkspaceColorRequest {
                        command_id: command_id.clone(),
                        color,
                    },
                )
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context.output.value(&response, || {
                println!("{}\t{}", response.color, response.id)
            })
        }
        WorkspaceCommand::Remove { id, command_id } => {
            let command_id = choose_command_id(command_id)?;
            let response = client
                .remove_workspace(
                    &id,
                    &RemoveWorkspaceRequest {
                        command_id: command_id.clone(),
                    },
                )
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context.output.value(&response, || {
                println!("{}\t{}", response.id, response.status)
            })
        }
    }
}
