use slop_client::DaemonClient;

use crate::Result;

use super::Context;

pub(super) async fn status(context: &Context) -> Result<()> {
    let client = DaemonClient::new(&context.connection.daemon)?;
    let health = client.health().await?;
    context.output.value(&health, || {
        println!("{} {}", health.service, health.version);
        println!("API version: {}", health.api_version);
        println!("Capabilities: {}", health.capabilities.join(", "));
    })
}

pub(super) async fn node(context: &Context) -> Result<()> {
    let client = context.client()?;
    let node = client.node().await?;
    context.output.value(&node, || {
        println!("{} ({})", node.name, node.node_id);
        println!("OS: {}", node.os);
    })
}

pub(super) async fn models(context: &Context) -> Result<()> {
    let client = context.client()?;
    let models = client.models().await?;
    context.output.value(&models, || {
        for model in &models {
            println!(
                "{}\t{}\t{}",
                if model.ready { "ready" } else { "unavailable" },
                model.id,
                model.display_name
            );
            if let Some(reason) = &model.reason {
                println!("  {reason}");
            }
        }
    })
}
