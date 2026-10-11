//! Native Sign in with ChatGPT for a daemon-owned, renewable connection.
//!
//! https://developers.openai.com/siwc/token-sharing-open-source/sign-in
//! https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};

use super::{ProviderError, ProviderId, transport};

const ISSUER: &str = "https://auth.openai.com";
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;

mod connection;
mod credentials;
mod identity;
mod login;
#[cfg(test)]
mod tests;

pub use connection::ChatGptConnection;
#[cfg(test)]
pub(in crate::providers) use connection::fixture_connection;
pub use login::ChatGptLogin;
