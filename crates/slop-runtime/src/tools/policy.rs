use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePolicy {
    pub root: String,
    pub allowed_tools: Vec<String>,
    pub shell_timeout_ms: u64,
    pub max_output_bytes: usize,
    pub max_tool_calls: u32,
    pub max_model_requests: u32,
}

impl WorkspacePolicy {
    pub fn validate(&self) -> std::io::Result<()> {
        if !Path::new(&self.root).is_absolute()
            || self.allowed_tools.is_empty()
            || self.allowed_tools.len() > NAMES.len()
            || self
                .allowed_tools
                .iter()
                .any(|name| !NAMES.contains(&name.as_str()))
            || self
                .allowed_tools
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.allowed_tools.len()
            || !(1..=300_000).contains(&self.shell_timeout_ms)
            || !(1024..=8 * 1024 * 1024).contains(&self.max_output_bytes)
            || !(1..=128).contains(&self.max_tool_calls)
            || !(1..=64).contains(&self.max_model_requests)
        {
            return Err(std::io::Error::other("invalid workspace policy"));
        }
        Ok(())
    }

    pub fn canonicalize(mut self) -> std::io::Result<Self> {
        self.validate()?;
        let root = std::fs::canonicalize(&self.root)?;
        if !root.is_dir() {
            return Err(std::io::Error::other("workspace is not a directory"));
        }
        self.root = root
            .to_str()
            .ok_or_else(|| std::io::Error::other("workspace path is not UTF-8"))?
            .to_owned();
        Ok(self)
    }
}
