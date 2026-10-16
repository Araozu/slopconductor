//! Provider credential and discovery DTOs.

pub const MAX_API_KEY_BYTES: usize = 16 * 1024;

mod set_api_key_request;
pub use set_api_key_request::SetApiKeyRequest;
mod provider_status;
pub use provider_status::ProviderStatus;
mod login_callback_request;
mod login_response;
pub use login_callback_request::LoginCallbackRequest;
pub use login_response::LoginResponse;
mod start_login_request;
pub use start_login_request::StartLoginRequest;
mod provider_account;
pub use provider_account::ProviderAccount;
mod select_auth_request;
pub use select_auth_request::SelectAuthRequest;
mod sign_out_response;
pub use sign_out_response::SignOutResponse;
mod provider_catalog;
pub use provider_catalog::ProviderCatalog;
mod login_status;
pub use login_status::LoginStatus;
