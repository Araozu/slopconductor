use super::Context;
use crate::{Result, args::ArtifactCommand};
use tokio::io::AsyncWriteExt;

pub(super) async fn run(command: ArtifactCommand, context: &Context) -> Result<()> {
    let client = context.client()?;
    match command {
        ArtifactCommand::Show { id } => {
            let metadata = client.artifact(&id).await?;
            context.output.value(&metadata, || {
                println!(
                    "{}\t{} bytes\t{}",
                    metadata.id, metadata.size_bytes, metadata.media_type
                )
            })?;
        }
        ArtifactCommand::Download { id, output } => {
            let output = std::path::absolute(output)?;
            let parent = output.parent().ok_or("output must name a file")?;
            let temp = tempfile::NamedTempFile::new_in(parent)?;
            let mut file = tokio::fs::File::from_std(temp.reopen()?);
            let mut stream = client.artifact_content(&id).await?;
            while let Some(chunk) = stream.next_chunk().await? {
                file.write_all(&chunk).await?;
            }
            file.sync_all().await?;
            drop(file);
            temp.persist_noclobber(&output)?;
            context.output.value(
                &serde_json::json!({"artifact":stream.metadata,"path":output}),
                || println!("{}", output.display()),
            )?;
        }
    }
    Ok(())
}
