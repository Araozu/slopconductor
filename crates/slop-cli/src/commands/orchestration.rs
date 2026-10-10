use super::Context;
use crate::{
    Result,
    args::{BatchCommand, RunCommand, TaskCommand},
    input::prompt_value,
    mutation::{choose_command_id, mutation_error},
};
use slop_protocol::{
    chat::{DeliveryMode, SendMessageRequest, TurnControlRequest},
    execution::GenerationSettings,
    orchestration::*,
    projects::ProjectWorkspaceRequest,
};
use std::{
    io::{Read, Write},
    path::Path,
};

pub(super) async fn task(command: TaskCommand, context: &Context) -> Result<()> {
    let client = context.client()?;
    match command {
        TaskCommand::Create {
            model,
            title,
            text,
            prompt_file,
            effort,
            max_output_tokens,
            project,
            base,
            tools,
            command_id,
        } => {
            let prompt = prompt_value(text, prompt_file).await?;
            let command_id = choose_command_id(command_id)?;
            let response = client
                .create_task(&CreateTaskRequest {
                    command_id: command_id.clone(),
                    spec: TaskSpec {
                        title,
                        prompt,
                        model,
                        settings: GenerationSettings {
                            max_output_tokens,
                            reasoning_effort: effort,
                        },
                        project: project.map(|project_id| ProjectWorkspaceRequest {
                            project_id,
                            base_ref: base,
                            allowed_tools: if tools.is_empty() {
                                vec!["read".into(), "write".into(), "edit".into(), "bash".into()]
                            } else {
                                tools
                            },
                        }),
                    },
                })
                .await
                .map_err(|e| mutation_error(e, &command_id))?;
            context.output.value(&response, || {
                println!(
                    "Task: {}\nRun: {}\nSession: {}\nTurn: {}",
                    response.task_id, response.run_id, response.session_id, response.turn_id
                )
            })
        }
        TaskCommand::Show { id } => {
            let response = client.task(&id).await?;
            context.output.value(&response, || show_task(&response))
        }
        TaskCommand::List { after, limit } => {
            let response = client.tasks(after, Some(limit)).await?;
            context.output.value(&response, || {
                for t in &response.items {
                    show_task(t);
                }
            })
        }
        TaskCommand::Runs { id, after, limit } => {
            let response = client.runs(&id, after, Some(limit)).await?;
            context.output.value(&response, || {
                for r in &response.items {
                    show_run(r);
                }
            })
        }
        TaskCommand::Events { id, after, limit } => {
            let response = client.task_events(&id, after, Some(limit)).await?;
            context.output.value(&response, || {
                for e in &response.items {
                    println!("{}\t{}\t{}\t{}", e.sequence, e.run_id, e.kind, e.status);
                }
            })
        }
        TaskCommand::Follow { id, after } => {
            let response = client.task(&id).await?;
            crate::follow::follow_turn(
                &client,
                &response.latest_run.session_id,
                &response.latest_run.turn.id,
                after,
                context.output,
            )
            .await
        }
        TaskCommand::Send {
            id,
            text,
            prompt_file,
            delivery,
            command_id,
        } => {
            let text = prompt_value(text, prompt_file).await?;
            let command_id = choose_command_id(command_id)?;
            let receipt = client
                .task_instruction(
                    &id,
                    &SendMessageRequest {
                        command_id: command_id.clone(),
                        text,
                        delivery: Some(if delivery == "immediate" {
                            DeliveryMode::Immediate
                        } else {
                            DeliveryMode::NextBoundary
                        }),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| mutation_error(e, &command_id))?;
            context.output.value(&receipt, || {
                println!(
                    "Instruction accepted for turn {}",
                    receipt.turn_id.as_deref().unwrap_or("pending")
                )
            })
        }
    }
}

pub(super) async fn run(command: RunCommand, context: &Context) -> Result<()> {
    let client = context.client()?;
    let (id, command_id, action) = match command {
        RunCommand::Show { id } => {
            let response = client.run(&id).await?;
            return context.output.value(&response, || show_run(&response));
        }
        RunCommand::Retry {
            id,
            command_id,
            acknowledge_unknown_effects,
        } => {
            let command_id = choose_command_id(command_id)?;
            let response = client
                .retry_run(
                    &id,
                    &RetryRunRequest {
                        command_id: command_id.clone(),
                        acknowledge_unknown_effects,
                    },
                )
                .await
                .map_err(|e| mutation_error(e, &command_id))?;
            return context.output.value(&response, || {
                println!(
                    "Task: {}\nRun: {}\nSession: {}",
                    response.task_id, response.run_id, response.session_id
                )
            });
        }
        RunCommand::Pause { id, command_id } => (id, command_id, "pause"),
        RunCommand::Resume { id, command_id } => (id, command_id, "resume"),
        RunCommand::Cancel { id, command_id } => (id, command_id, "cancel"),
    };
    let command_id = choose_command_id(command_id)?;
    let response = client
        .control_run(
            &id,
            action,
            &TurnControlRequest {
                command_id: command_id.clone(),
            },
        )
        .await
        .map_err(|e| mutation_error(e, &command_id))?;
    context
        .output
        .value(&response, || println!("{action} accepted for run {id}"))
}

pub(super) async fn batch(command: BatchCommand, context: &Context) -> Result<()> {
    let client = context.client()?;
    match command {
        BatchCommand::Preview { file, output } => {
            let response = client.preview_batch(&read_spec(&file)?).await?;
            if let Some(path) = output {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?;
                serde_json::to_writer_pretty(&mut file, &response.spec)?;
                file.write_all(b"\n")?;
            }
            context.output.value(&response, || {
                println!(
                    "{} combinations; at most {} active runs",
                    response.combinations.len(),
                    response.spec.max_concurrent_runs
                );
                for c in &response.combinations {
                    println!(
                        "{}\tprompt {}\t{}\t{:?}",
                        c.index, c.prompt_index, c.spec.model, c.spec.settings
                    );
                }
            })
        }
        BatchCommand::Submit { file, command_id } => {
            let command_id = choose_command_id(command_id)?;
            let response = client
                .create_batch(&CreateBatchRequest {
                    command_id: command_id.clone(),
                    spec: read_spec(&file)?,
                })
                .await
                .map_err(|e| mutation_error(e, &command_id))?;
            context.output.value(&response, || {
                println!(
                    "Batch: {}\nMembers: {}",
                    response.batch_id,
                    response.members.len()
                )
            })
        }
        BatchCommand::Show { id } => {
            let response = client.batch(&id).await?;
            context.output.value(&response, || {
                println!(
                    "{}\t{}\t{:?}",
                    response.id, response.spec.name, response.statuses
                )
            })
        }
        BatchCommand::List { after, limit } => {
            let response = client.batches(after, Some(limit)).await?;
            context.output.value(&response, || {
                for b in &response.items {
                    println!("{}\t{}\t{:?}", b.id, b.spec.name, b.statuses);
                }
            })
        }
        BatchCommand::Members { id, after, limit } => {
            let response = client.batch_members(&id, after, Some(limit)).await?;
            context.output.value(&response, || {
                for t in &response.items {
                    show_task(t);
                }
            })
        }
        BatchCommand::Results { id, after, limit } => {
            let response = client.batch_results(&id, after, Some(limit)).await?;
            context.output.value(&response, || {
                for r in &response.items {
                    show_task(&r.task);
                    if let Some(o) = &r.output {
                        println!("{}", o.text);
                    }
                }
            })
        }
        BatchCommand::Export { id, output } => {
            let mut file = std::io::BufWriter::new(
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&output)?,
            );
            let mut after = None;
            let mut count = 0;
            let result: Result<()> = async {
                loop {
                    let page = client.batch_results(&id, after, Some(50)).await?;
                    for value in &page.items {
                        serde_json::to_writer(&mut file, value)?;
                        file.write_all(b"\n")?;
                        count += 1;
                    }
                    if page.next_after.is_none() {
                        break;
                    }
                    after = page.next_after;
                }
                file.flush()?;
                Ok(())
            }
            .await;
            drop(file);
            if let Err(e) = result {
                let _ = std::fs::remove_file(&output);
                return Err(e);
            }
            context.output.value(
                &serde_json::json!({"batch_id":id,"members":count,"output":output}),
                || println!("Exported {count} members to {}", output.display()),
            )
        }
        BatchCommand::Retry {
            id,
            indices,
            command_id,
            acknowledge_unknown_effects,
        } => {
            let command_id = choose_command_id(command_id)?;
            let response = client
                .retry_batch(
                    &id,
                    &RetryBatchRequest {
                        command_id: command_id.clone(),
                        indices,
                        acknowledge_unknown_effects,
                    },
                )
                .await
                .map_err(|e| mutation_error(e, &command_id))?;
            context.output.value(&response, || {
                println!(
                    "Batch: {}\nRetried members: {}",
                    response.batch_id,
                    response.members.len()
                )
            })
        }
    }
}

fn read_spec(path: &Path) -> Result<BatchSpec> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("batch input exceeds 2 MiB".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn show_task(t: &TaskResponse) {
    println!(
        "{}\t{}\t{}\t{}",
        t.id, t.latest_run.turn.status, t.spec.model, t.latest_run.id
    );
}
fn show_run(r: &RunResponse) {
    println!(
        "{}\tattempt {}\t{}\nSession: {}\nTurn: {}",
        r.id, r.attempt, r.turn.status, r.session_id, r.turn.id
    );
    if let Some(w) = &r.workspace_id {
        println!("Workspace: {w}");
    }
    if let Some(e) = &r.turn.error_code {
        println!("Error: {e}");
    }
    if r.effects_unknown {
        println!("Effects unknown: true");
    }
}
