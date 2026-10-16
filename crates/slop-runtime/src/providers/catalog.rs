//! Public, provider-scoped Models.dev metadata, independent of credentials.
//!
//! The bundled subset keeps offline execution available. A daemon can replace
//! it atomically with a bounded live snapshot; failed refreshes retain the last
//! good catalog. Authenticated `/models` probes remain separate access evidence.
//! Bundled source: https://models.dev/api.json, retrieved 2026-10-16; see
//! docs/model-catalog.md for upstream revision, refresh policy, and limitations.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{OnceLock, RwLock},
    time::Duration,
};

use serde_json::Value;
use slop_core::provider::{ProviderId, is_valid_model_id};

use super::{Provider, ProviderError, WireProtocol, inference::ModelCapabilities};

pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
const MAX_MODELS: usize = 512;

#[derive(Debug, Clone)]
pub struct CatalogModel {
    pub id: String,
    pub display_name: String,
    pub wire: WireProtocol,
}

#[derive(Debug, Clone)]
struct Metadata {
    display_name: String,
    wire: Option<WireProtocol>,
    capabilities: ModelCapabilities,
    deprecated: bool,
}

#[derive(Default)]
struct Catalog(HashMap<ProviderId, BTreeMap<String, Metadata>>);

fn current() -> &'static RwLock<Catalog> {
    static CURRENT: OnceLock<RwLock<Catalog>> = OnceLock::new();
    CURRENT.get_or_init(|| {
        RwLock::new(
            parse(include_bytes!("catalog/models-dev.json"))
                .expect("bundled Models.dev catalog is valid"),
        )
    })
}

fn source_id(provider: ProviderId) -> Option<&'static str> {
    match provider {
        ProviderId::OpencodeGo => Some("opencode-go"),
        ProviderId::OpencodeZen => Some("opencode"),
        _ => None,
    }
}

pub(super) fn wire(provider: ProviderId, model: &str) -> Option<Option<WireProtocol>> {
    let catalog = current().read().expect("catalog read lock");
    catalog
        .0
        .get(&provider)?
        .get(model)
        .map(|m| if m.deprecated { None } else { m.wire })
}

pub(super) fn capabilities(provider: ProviderId, model: &str) -> Option<ModelCapabilities> {
    let catalog = current().read().expect("catalog read lock");
    let metadata = catalog.0.get(&provider)?.get(model)?;
    Some(metadata.capabilities.clone())
}

/// Executable public metadata plus the verified compiled fallback mappings.
pub fn models(provider: &dyn Provider) -> Vec<CatalogModel> {
    let catalog = current().read().expect("catalog read lock");
    let discovered = catalog.0.get(&provider.id());
    let mut models: BTreeMap<_, _> = provider
        .models()
        .iter()
        .map(|m| {
            (
                m.id.to_owned(),
                CatalogModel {
                    id: m.id.to_owned(),
                    display_name: m.display_name.to_owned(),
                    wire: m.wire,
                },
            )
        })
        .collect();
    if let Some(discovered) = discovered {
        for (id, metadata) in discovered {
            if let Some(wire) = metadata.wire.filter(|_| !metadata.deprecated) {
                models.insert(
                    id.clone(),
                    CatalogModel {
                        id: id.clone(),
                        display_name: metadata.display_name.clone(),
                        wire,
                    },
                );
            }
        }
    }
    models.into_values().collect()
}

/// Refresh public metadata without sending provider credentials or following
/// redirects. The URL is explicit so controlled/offline gateways can supply
/// their own Models.dev-shaped metadata.
pub async fn refresh(url: &str) -> Result<(), ProviderError> {
    let catalog = fetch(url).await?;
    *current().write().expect("catalog write lock") = catalog;
    Ok(())
}

async fn fetch(url: &str) -> Result<Catalog, ProviderError> {
    let url = super::opencode::validate_base_url(url)?;
    let http = reqwest::Client::builder()
        .user_agent(super::opencode::USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .build()?;
    let mut response = http.get(url).send().await?;
    if !response.status().is_success() {
        return Err(ProviderError::InvalidRequest(
            "Models.dev catalog request failed",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_CATALOG_BYTES {
            return Err(ProviderError::LimitExceeded {
                detail: "Models.dev catalog exceeds byte bound",
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    parse(&bytes)
}

fn parse(bytes: &[u8]) -> Result<Catalog, ProviderError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| ProviderError::InvalidRequest("invalid Models.dev catalog"))?;
    let mut catalog = Catalog::default();
    for provider in [ProviderId::OpencodeGo, ProviderId::OpencodeZen] {
        let source = &value[source_id(provider).expect("gateway source")];
        let default_npm = source["npm"]
            .as_str()
            .filter(|npm| !npm.is_empty() && npm.len() <= 128)
            .ok_or(ProviderError::InvalidRequest(
                "missing Models.dev API mapping",
            ))?;
        let models = source["models"]
            .as_object()
            .filter(|models| !models.is_empty() && models.len() <= MAX_MODELS)
            .ok_or(ProviderError::InvalidRequest(
                "missing or oversized Models.dev provider catalog",
            ))?;
        let mut parsed = BTreeMap::new();
        for (id, model) in models {
            if !is_valid_model_id(id) || model["id"].as_str() != Some(id) {
                continue;
            }
            let Some(name) = model["name"].as_str().filter(|name| {
                !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
            }) else {
                continue;
            };
            let npm = model
                .get("provider")
                .and_then(|provider| provider.get("npm"))
                .map_or(Some(default_npm), Value::as_str);
            let text_output = model["modalities"]["output"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "text"));
            let wire = super::provider(provider)
                .and_then(|p| p.models().iter().find(|m| m.id == id))
                .map(|m| m.wire)
                .or(match npm.filter(|_| text_output) {
                    Some("@ai-sdk/openai") => Some(WireProtocol::OpenAiResponses),
                    Some("@ai-sdk/openai-compatible") => Some(WireProtocol::OpenAiChatCompletions),
                    Some("@ai-sdk/anthropic") => Some(WireProtocol::AnthropicMessages),
                    _ => None,
                });
            let mut reasoning_efforts = Vec::new();
            if model["reasoning"].as_bool() == Some(true)
                && wire.is_some()
                && let Some(options) = model["reasoning_options"].as_array()
            {
                for option in options
                    .iter()
                    .take(16)
                    .filter(|option| option["type"] == "effort")
                {
                    if let Some(values) = option["values"].as_array() {
                        for effort in values.iter().take(32).filter_map(Value::as_str) {
                            // Null/default are absence of an override, not wire efforts.
                            if reasoning_efforts.len() < 32
                                && !effort.is_empty()
                                && effort.len() <= 32
                                && effort.bytes().all(|b| {
                                    b.is_ascii_lowercase()
                                        || b.is_ascii_digit()
                                        || matches!(b, b'_' | b'-')
                                })
                                && !matches!(effort, "default" | "null" | "off")
                                && !reasoning_efforts.iter().any(|value| value == effort)
                            {
                                reasoning_efforts.push(effort.to_owned());
                            }
                        }
                    }
                }
            }
            let positive = |field: &str| model["limit"][field].as_u64().filter(|n| *n > 0);
            parsed.insert(
                id.clone(),
                Metadata {
                    display_name: name.to_owned(),
                    wire,
                    deprecated: model["status"].as_str() == Some("deprecated"),
                    capabilities: ModelCapabilities {
                        tools: wire.is_some() && model["tool_call"].as_bool() == Some(true),
                        reasoning_efforts,
                        context_window_tokens: positive("context"),
                        max_input_tokens: positive("input"),
                    },
                },
            );
        }
        if parsed.is_empty() {
            return Err(ProviderError::InvalidRequest(
                "empty Models.dev provider catalog",
            ));
        }
        catalog.0.insert(provider, parsed);
    }
    Ok(catalog)
}

#[cfg(test)]
mod tests;
