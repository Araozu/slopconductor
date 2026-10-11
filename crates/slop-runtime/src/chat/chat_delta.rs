#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatDelta {
    pub session_id: String,
    pub turn_id: String,
    pub text: String,
    pub message_id: Option<String>,
    pub block_id: Option<String>,
    pub request_id: Option<String>,
    pub invocation_id: Option<String>,
    pub stream_id: String,
    pub chunk_index: u64,
    pub kind: String,
}
