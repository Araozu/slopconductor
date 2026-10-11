use super::super::Usage;

#[derive(Debug, Clone)]
pub enum ProviderEvent {
    TextDelta {
        block_index: usize,
        text: String,
    },
    ToolArgumentsDelta {
        block_index: usize,
        fragment: String,
    },
    UsageUpdated {
        usage: Usage,
    },
}
