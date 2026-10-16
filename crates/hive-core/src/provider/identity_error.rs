use std::fmt;

/// Error for unknown provider names or invalid model identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderIdentityError(pub &'static str);

impl fmt::Display for ProviderIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ProviderIdentityError {}
