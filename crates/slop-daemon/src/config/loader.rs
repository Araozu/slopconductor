const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const DEFAULT_LISTEN: &str = "127.0.0.1:7331";
const DEFAULT_QUEUE_CAPACITY: usize = 128;
const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 10_000;

use std::{
    fs::{self, File},
    io::Read,
    path::PathBuf,
    time::Duration,
};

use slop_core::provider::ProviderModelRef;
use slop_runtime::providers::opencode_go::OpencodeGoClient;

use super::file_config::FileConfig;
use super::{Config, ConfigEnvironment, ConfigError, StartupArgs};

#[cfg(test)]
pub fn load_with_environment(
    args: StartupArgs,
    environment: &ConfigEnvironment,
) -> Result<Config, ConfigError> {
    load_with_environment_and_name(args, None, environment)
}

pub fn load_with_environment_and_name(
    args: StartupArgs,
    node_name_override: Option<String>,
    environment: &ConfigEnvironment,
) -> Result<Config, ConfigError> {
    let explicit_config = args.config.is_some();
    let config_path = match args.config {
        Some(path) => Some(require_absolute(path, "--config/SLOP_CONFIG")?),
        None => default_config_path(environment)?,
    };
    let data_dir = match args.data_dir {
        Some(path) => require_absolute(path, "--data-dir/SLOP_DATA_DIR")?,
        None => default_data_dir(environment)?,
    };

    let file = match config_path {
        Some(path) if path.exists() => {
            let metadata = fs::metadata(&path).map_err(|source| ConfigError::Read {
                path: path.clone(),
                source,
            })?;
            if !metadata.is_file() {
                return Err(ConfigError::Invalid(format!(
                    "configuration path {} is not a regular file",
                    path.display()
                )));
            }
            if metadata.len() > MAX_CONFIG_BYTES {
                return Err(ConfigError::Invalid(format!(
                    "configuration file {} exceeds {} bytes",
                    path.display(),
                    MAX_CONFIG_BYTES
                )));
            }
            let mut contents = String::new();
            File::open(&path)
                .and_then(|file| {
                    file.take(MAX_CONFIG_BYTES + 1)
                        .read_to_string(&mut contents)
                })
                .map_err(|source| ConfigError::Read {
                    path: path.clone(),
                    source,
                })?;
            if contents.len() as u64 > MAX_CONFIG_BYTES {
                return Err(ConfigError::Invalid(format!(
                    "configuration file {} exceeds {} bytes",
                    path.display(),
                    MAX_CONFIG_BYTES
                )));
            }
            toml::from_str(&contents).map_err(|_| ConfigError::Parse(path))?
        }
        Some(path) if explicit_config => {
            return Err(ConfigError::Invalid(format!(
                "explicit configuration file {} does not exist",
                path.display()
            )));
        }
        _ => FileConfig::default(),
    };

    let listen = args
        .listen
        .or(file.listen)
        .unwrap_or_else(|| DEFAULT_LISTEN.parse().expect("constant socket address"));
    if !listen.ip().is_loopback() {
        return Err(ConfigError::Invalid(
            "the bootstrap daemon only supports loopback listeners".into(),
        ));
    }
    let peer_listen = args.peer_listen.or(file.peer_listen);
    if peer_listen.is_some_and(|address| !address.ip().is_loopback()) {
        return Err(ConfigError::Invalid(
            "peer_listen must be a loopback address".into(),
        ));
    }
    let queue_capacity = file
        .database_queue_capacity
        .unwrap_or(DEFAULT_QUEUE_CAPACITY);
    let busy_timeout_ms = file
        .database_busy_timeout_ms
        .unwrap_or(DEFAULT_BUSY_TIMEOUT_MS);
    let shutdown_timeout_ms = file
        .shutdown_timeout_ms
        .unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT_MS);
    let provider_base_url = args.provider_base_url.or(file.provider_base_url);
    if let Some(base_url) = provider_base_url.as_deref() {
        OpencodeGoClient::validate_base_url(base_url).map_err(|_| {
            ConfigError::Invalid("provider_base_url is not an allowed endpoint".into())
        })?;
    }
    let opencode_zen_base_url = args.opencode_zen_base_url.or(file.opencode_zen_base_url);
    if let Some(base_url) = opencode_zen_base_url.as_deref() {
        OpencodeGoClient::validate_base_url(base_url).map_err(|_| {
            ConfigError::Invalid("opencode_zen_base_url is not an allowed endpoint".into())
        })?;
    }
    let codex_base_url = args.codex_base_url.or(file.codex_base_url);
    if let Some(base_url) = codex_base_url.as_deref() {
        OpencodeGoClient::validate_base_url(base_url).map_err(|_| {
            ConfigError::Invalid("codex_base_url is not an allowed endpoint".into())
        })?;
    }
    let default_model = args
        .default_model
        .or(file.default_model)
        .unwrap_or_else(|| slop_runtime::chat::DEFAULT_MODEL.to_owned());
    let model_ref = default_model.parse::<ProviderModelRef>().map_err(|_| {
        ConfigError::Invalid("default_model must be a supported opencode-go/model id".into())
    })?;
    let supported_default = slop_runtime::providers::provider(model_ref.provider())
        .is_some_and(|provider| provider.wire_protocol(model_ref.model()).is_ok());
    if !supported_default {
        return Err(ConfigError::Invalid(
            "default_model must identify a model supported by a compiled provider".into(),
        ));
    }
    let max_output_tokens = file
        .max_output_tokens
        .unwrap_or(slop_runtime::chat::DEFAULT_OUTPUT_TOKENS);
    if !(1..=65_536).contains(&max_output_tokens) {
        return Err(ConfigError::Invalid(
            "max_output_tokens must be between 1 and 65536".into(),
        ));
    }
    let execution_concurrency = file
        .execution_concurrency
        .unwrap_or(slop_runtime::chat::DEFAULT_CONCURRENCY);
    let context_token_budget = file
        .context_token_budget
        .unwrap_or(slop_runtime::agent::context::DEFAULT_TOKEN_BUDGET);
    if !(8_192..=2_000_000).contains(&context_token_budget) {
        return Err(ConfigError::Invalid(
            "context_token_budget must be between 8192 and 2000000".into(),
        ));
    }
    if !(1..=64).contains(&execution_concurrency) {
        return Err(ConfigError::Invalid(
            "execution_concurrency must be between 1 and 64".into(),
        ));
    }
    if !(1..=4_096).contains(&queue_capacity) {
        return Err(ConfigError::Invalid(
            "database_queue_capacity must be between 1 and 4096".into(),
        ));
    }
    if !(1..=60_000).contains(&busy_timeout_ms) {
        return Err(ConfigError::Invalid(
            "database_busy_timeout_ms must be between 1 and 60000".into(),
        ));
    }
    if !(100..=300_000).contains(&shutdown_timeout_ms) {
        return Err(ConfigError::Invalid(
            "shutdown_timeout_ms must be between 100 and 300000".into(),
        ));
    }
    let node_name = node_name_override
        .or(file.node_name)
        .map(|name| name.trim().to_owned());
    if node_name.as_ref().is_some_and(|name| {
        name.is_empty() || name.len() > 128 || name.chars().any(char::is_control)
    }) {
        return Err(ConfigError::Invalid(
            "node_name must contain 1 to 128 non-control characters".into(),
        ));
    }
    Ok(Config {
        listen,
        peer_listen,
        node_name,
        database_queue_capacity: queue_capacity,
        database_busy_timeout: Duration::from_millis(busy_timeout_ms),
        shutdown_timeout: Duration::from_millis(shutdown_timeout_ms),
        data_dir,
        provider_base_url,
        opencode_zen_base_url,
        codex_base_url,
        default_model,
        max_output_tokens,
        context_token_budget,
        execution_concurrency,
    })
}

fn require_absolute(path: PathBuf, source: &str) -> Result<PathBuf, ConfigError> {
    if !path.is_absolute() {
        return Err(ConfigError::Invalid(format!(
            "{source} must be an absolute path"
        )));
    }
    Ok(path)
}

fn default_config_path(env: &ConfigEnvironment) -> Result<Option<PathBuf>, ConfigError> {
    #[cfg(windows)]
    {
        env.local_app_data.as_ref().filter(|p| p.is_absolute() && !p.as_os_str().is_empty())
            .map(|base| Some(base.join("slopconductor/config.toml")))
            .ok_or_else(|| ConfigError::Invalid("LOCALAPPDATA must contain an absolute path when no config override is supplied".into()))
    }
    #[cfg(not(windows))]
    {
        if let Some(base) = env
            .xdg_config_home
            .as_ref()
            .filter(|p| p.is_absolute() && !p.as_os_str().is_empty())
        {
            return Ok(Some(base.join("slopconductor/config.toml")));
        }
        Ok(Some(
            home_path(env)?.join(".config/slopconductor/config.toml"),
        ))
    }
}

fn default_data_dir(env: &ConfigEnvironment) -> Result<PathBuf, ConfigError> {
    #[cfg(windows)]
    {
        env.local_app_data
            .as_ref()
            .filter(|p| p.is_absolute() && !p.as_os_str().is_empty())
            .map(|base| base.join("slopconductor"))
            .ok_or_else(|| {
                ConfigError::Invalid(
                    "LOCALAPPDATA must contain an absolute path when no data override is supplied"
                        .into(),
                )
            })
    }
    #[cfg(not(windows))]
    {
        let base = match env
            .xdg_data_home
            .as_ref()
            .filter(|p| p.is_absolute() && !p.as_os_str().is_empty())
        {
            Some(path) => path.clone(),
            None => home_path(env)?.join(".local/share"),
        };
        Ok(base.join("slopconductor"))
    }
}

#[cfg(not(windows))]
fn home_path(env: &ConfigEnvironment) -> Result<PathBuf, ConfigError> {
    env.home
        .as_ref()
        .filter(|p| p.is_absolute() && !p.as_os_str().is_empty())
        .cloned()
        .ok_or_else(|| {
            ConfigError::Invalid(
                "HOME must contain an absolute path when an XDG directory is not set".into(),
            )
        })
}
