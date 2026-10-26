#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub tools: bool,
    pub reasoning_efforts: Vec<String>,
    pub context_window_tokens: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub image_input: bool,
}
