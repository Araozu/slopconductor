use super::RoleKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMessage {
    pub role: RoleKind,
    pub text: String,
}
