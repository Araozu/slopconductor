#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleKind {
    System,
    Developer,
    User,
    Assistant,
}
