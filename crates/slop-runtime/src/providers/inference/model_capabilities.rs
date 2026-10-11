#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub tools: bool,
    pub reasoning_efforts: Vec<String>,
}
