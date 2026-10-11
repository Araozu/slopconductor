use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DeliveryMode {
    #[default]
    AfterTurn,
    NextBoundary,
    Immediate,
}
