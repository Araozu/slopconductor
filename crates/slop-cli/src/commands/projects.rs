use super::Context;
use crate::{
    Result,
    args::{ProjectCommand, WorkspaceCommand},
    mutation::{choose_command_id, mutation_error},
};
use slop_protocol::projects::{RegisterProjectRequest, RemoveWorkspaceRequest};

pub(super) async fn project(command: ProjectCommand, context: &Context) -> Result<()> {
    let client = context.client()?;
    match command {
        ProjectCommand::Register { path, command_id } => {
            if !path.is_absolute() {
                return Err("project path must be absolute on the daemon's machine".into());
            }
            let command_id = choose_command_id(command_id)?;
            let response = client
                .register_project(&RegisterProjectRequest {
                    command_id: command_id.clone(),
                    path: path.to_str().ok_or("project path must be UTF-8")?.into(),
                })
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context
                .output
                .value(&response, || println!("{}\t{}", response.id, response.path))
        }
        ProjectCommand::List { after, limit } => {
            let response = client.projects(after, Some(limit)).await?;
            context.output.value(&response, || {
                for project in &response.items {
                    println!("{}\t{}", project.id, project.path);
                }
            })
        }
        ProjectCommand::Show { id } => {
            let response = client.project(&id).await?;
            context.output.value(&response, || {
                println!(
                    "{}\t{}\nGit common directory: {}",
                    response.id, response.path, response.git_common_dir
                )
            })
        }
        ProjectCommand::Workspaces { id, after, limit } => {
            let response = client.project_workspaces(&id, after, Some(limit)).await?;
            context.output.value(&response, || {
                for workspace in &response.items {
                    println!(
                        "{}\t{}\t{}\t{}",
                        workspace.id, workspace.status, workspace.session_id, workspace.path
                    );
                }
            })
        }
        ProjectCommand::Events { id, after, limit } => {
            let response = client.project_events(&id, after, Some(limit)).await?;
            context.output.value(&response, || {
                for event in &response.items {
                    println!(
                        "{}\t{}\t{}",
                        event.sequence,
                        event.kind,
                        event.workspace_id.as_deref().unwrap_or("-")
                    );
                }
            })
        }
    }
}

pub(super) async fn workspace(command: WorkspaceCommand, context: &Context) -> Result<()> {
    let client = context.client()?;
    match command {
        WorkspaceCommand::Show { id } => {
            let response = client.workspace(&id).await?;
            context.output.value(&response, || {
                println!(
                    "{}\t{}\nSession: {}\nBase: {}\nPath: {}",
                    response.id,
                    response.status,
                    response.session_id,
                    response.base_commit,
                    response.path
                );
                if let Some(error) = &response.error_code {
                    println!(
                        "Error: {error}\nEffects unknown: {}",
                        response.effects_unknown
                    );
                }
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
