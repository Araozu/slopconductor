//! Explicit workspace capabilities and bounded native tool supervision.
//! A workspace limits file-tool authority; shell access is not an OS sandbox.

use std::{
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::providers::inference::ToolDefinition;

pub const NAMES: &[&str] = &["read", "write", "edit", "bash"];
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
pub const PREVIEW_BYTES: usize = 16 * 1024;
const MAX_READ_BYTES: usize = 256 * 1024;
const MAX_READ_LINES: usize = 2000;

mod arguments;
mod outcome;
mod policy;
mod service;
#[cfg(test)]
mod tests;

#[cfg(test)]
use arguments::replace_file;
use arguments::{BashArgs, file_tool};
pub use arguments::{definitions, validate_arguments};
pub use outcome::{Artifact, ToolOutcome};
pub use policy::WorkspacePolicy;
pub use service::{ToolService, sniff_image_media_type};
