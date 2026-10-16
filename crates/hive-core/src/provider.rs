//! Provider and model identities for Hive.
//!
//! These are transport-independent domain values. HTTP endpoints, credentials,
//! and wire payloads belong in `hive-runtime`; versioned DTOs belong in
//! `hive-protocol`. This module only names the static set of supported
//! providers and validates opaque model identifiers.

mod identity_error;
mod model_ref;
mod provider_id;
#[cfg(test)]
mod tests;

pub use identity_error::ProviderIdentityError;
pub use model_ref::{MAX_MODEL_ID_LEN, ProviderModelRef, is_valid_model_id};
pub use provider_id::ProviderId;
