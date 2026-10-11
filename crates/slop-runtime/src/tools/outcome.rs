use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artifact {
    pub id: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub media_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolOutcome {
    pub status: String,
    pub output: String,
    pub artifacts: Vec<Artifact>,
    pub error_code: Option<String>,
    pub effects_unknown: bool,
}

impl ToolOutcome {
    pub fn failed(code: &str, effects_unknown: bool) -> Self {
        Self {
            status: "failed".into(),
            output: format!("Tool failed: {code}."),
            artifacts: Vec::new(),
            error_code: Some(code.into()),
            effects_unknown,
        }
    }
}
