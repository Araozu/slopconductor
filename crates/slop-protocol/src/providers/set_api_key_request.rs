use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetApiKeyRequest {
    pub api_key: String,
}

impl std::fmt::Debug for SetApiKeyRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetApiKeyRequest([REDACTED])")
    }
}
