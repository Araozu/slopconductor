use crate::providers::inference::GenerationSettings;

#[derive(Debug, Clone)]
pub struct RequestIntent {
    pub id: String,
    pub message_id: String,
    pub ordinal: u32,
    pub requested_model: String,
    pub requested_settings: GenerationSettings,
    pub settings: GenerationSettings,
}
