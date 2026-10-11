/// Authentication implemented by a compiled-in adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    ApiKey,
    ChatGptSubscription,
}
