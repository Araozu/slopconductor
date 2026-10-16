use super::credentials::auth_error;
use super::*;

pub(super) async fn validate_identity(
    http: &reqwest::Client,
    token: &str,
    client_id: &str,
    nonce: &str,
) -> Result<String, ProviderError> {
    #[derive(Deserialize)]
    struct Discovery {
        jwks_uri: String,
    }
    let response = http
        .get(format!("{ISSUER}/.well-known/openid-configuration"))
        .send()
        .await?;
    let body = transport::read_body_limited(super::ProviderId::Codex, response).await?;
    let discovery: Discovery =
        serde_json::from_str(&body).map_err(|_| auth_error("invalid OIDC discovery"))?;
    let url =
        reqwest::Url::parse(&discovery.jwks_uri).map_err(|_| auth_error("invalid JWKS URL"))?;
    if url.scheme() != "https"
        || url.host_str() != Some("auth.openai.com")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(auth_error("untrusted JWKS endpoint"));
    }
    let response = http.get(url).send().await?;
    let body = transport::read_body_limited(super::ProviderId::Codex, response).await?;
    let jwks: JwkSet =
        serde_json::from_str(&body).map_err(|_| auth_error("invalid JWKS response"))?;
    verify_identity(token, client_id, nonce, &jwks)
}

pub(super) fn verify_identity(
    token: &str,
    client_id: &str,
    nonce: &str,
    jwks: &JwkSet,
) -> Result<String, ProviderError> {
    #[derive(Deserialize)]
    struct Claims {
        sub: String,
        nonce: String,
    }
    let header = decode_header(token).map_err(|_| auth_error("invalid ID token"))?;
    if header.alg != Algorithm::RS256 {
        return Err(auth_error("unsupported ID token signature"));
    }
    let key = header
        .kid
        .as_deref()
        .and_then(|kid| jwks.find(kid))
        .ok_or_else(|| auth_error("ID token signing key is missing"))?;
    let key = DecodingKey::from_jwk(key).map_err(|_| auth_error("invalid signing key"))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client_id]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = decode::<Claims>(token, &key, &validation)
        .map_err(|_| auth_error("ID token validation failed"))?
        .claims;
    if claims.nonce != nonce || claims.sub.is_empty() {
        return Err(auth_error("ID token identity or nonce does not match"));
    }
    Ok(claims.sub)
}
