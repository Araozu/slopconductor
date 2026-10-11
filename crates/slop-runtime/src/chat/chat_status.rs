#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatStatus {
    Completed,
    Incomplete,
    Interrupted,
    Canceled,
    Failed,
}
