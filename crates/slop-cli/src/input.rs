use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

use crate::Result;

const MAX_PROMPT_BYTES: usize = 64 * 1024;

pub async fn read_interactive_prompt(
    stdin: &mut tokio::io::BufReader<tokio::io::Stdin>,
) -> Result<PromptInput> {
    eprint!("you> ");
    io::stderr().flush()?;
    let mut line = String::new();
    use tokio::io::AsyncBufReadExt;
    let read = stdin.read_line(&mut line).await?;
    if read == 0 || line.trim() == "/exit" {
        return Ok(PromptInput::Exit);
    }
    if let Some(rest) = line.trim().strip_prefix("/cancel") {
        return Ok(PromptInput::Cancel(
            (!rest.trim().is_empty()).then(|| rest.trim().to_owned()),
        ));
    }
    Ok(PromptInput::Text(validate_prompt(
        line.trim_end().to_owned(),
    )?))
}

pub enum PromptInput {
    Exit,
    Cancel(Option<String>),
    Text(String),
}

pub async fn prompt_value(text: Option<String>, path: Option<PathBuf>) -> Result<String> {
    prompt_value_optional(text, path)
        .await?
        .ok_or_else(|| "a prompt is required".into())
}

pub async fn prompt_value_optional(
    text: Option<String>,
    path: Option<PathBuf>,
) -> Result<Option<String>> {
    let value = if let Some(text) = text {
        Some(text)
    } else if let Some(path) = path {
        Some(read_bounded_text(&path)?)
    } else if !io::stdin().is_terminal() {
        Some(read_bounded_stdin().await?)
    } else {
        None
    };
    value.map(validate_prompt).transpose()
}

fn read_bounded_text(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut bytes = Vec::with_capacity(MAX_PROMPT_BYTES.min(4096));
    std::fs::File::open(path)
        .and_then(|file| {
            file.take((MAX_PROMPT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| format!("could not read prompt file {}: {error}", path.display()))?;
    let value = String::from_utf8(bytes).map_err(|_| "prompt input is not UTF-8")?;
    validate_prompt(value)
}

async fn read_bounded_stdin() -> Result<String> {
    let mut bytes = Vec::new();
    use tokio::io::AsyncReadExt;
    tokio::io::stdin()
        .take((MAX_PROMPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    let value = String::from_utf8(bytes).map_err(|_| "prompt input is not UTF-8")?;
    validate_prompt(value)
}

fn validate_prompt(value: String) -> Result<String> {
    if value.len() > MAX_PROMPT_BYTES {
        Err("prompt exceeds the 64 KiB input limit".into())
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_limit_is_checked_in_bytes() {
        assert_eq!(
            validate_prompt("x".repeat(MAX_PROMPT_BYTES)).unwrap().len(),
            MAX_PROMPT_BYTES
        );
        assert!(validate_prompt("x".repeat(MAX_PROMPT_BYTES + 1)).is_err());
        assert!(validate_prompt("☃".repeat(MAX_PROMPT_BYTES / 3 + 1)).is_err());
    }
}
