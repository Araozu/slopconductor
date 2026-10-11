/// A bounded Git operation failure with a stable API error code.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{code}")]
pub struct GitError {
    pub code: &'static str,
    pub effects_unknown: bool,
}

impl GitError {
    pub(super) fn known(code: &'static str) -> Self {
        Self {
            code,
            effects_unknown: false,
        }
    }
}
