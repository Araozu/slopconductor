use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ProviderAccount {
    pub id: String,
    pub active: bool,
    pub authenticated: bool,
}
