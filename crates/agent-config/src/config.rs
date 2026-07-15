use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;
use tracing_subscriber::EnvFilter;
use url::Url;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-5.5";
const DEFAULT_API_TYPE: &str = "completions";
const DEFAULT_LOG_LEVEL: &str = "info";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiType {
    Completions,
    Responses,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    XHigh,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppConfig {
    pub provider: ProviderConfig,
    pub logging: LoggingConfig,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ProviderConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub api_type: ApiType,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderConfig")
            .field("api_key", &"[REDACTED]")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_type", &self.api_type)
            .field("reasoning_effort", &self.reasoning_effort)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoggingConfig {
    pub level: String,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Unable to determine the home directory; cannot locate the configuration file")]
    HomeDirectoryUnavailable,
    #[error(
        "Configuration file not found: {path}\nCreate it and set a non-empty api_key under [provider]."
    )]
    MissingFile { path: PathBuf },
    #[error("Unable to read configuration file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Failed to parse configuration file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("Configuration file {path} is missing required field {key}")]
    Missing { path: PathBuf, key: &'static str },
    #[error("Invalid value for {key} in configuration file {path}: {message}")]
    Invalid {
        path: PathBuf,
        key: &'static str,
        message: String,
    },
}

impl AppConfig {
    pub fn load_default() -> Result<Self, ConfigError> {
        Self::load_from(default_path()?)
    }

    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref().to_path_buf();
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(ConfigError::MissingFile { path });
            }
            Err(source) => return Err(ConfigError::Read { path, source }),
        };
        let raw: RawConfig = toml::from_str(&contents).map_err(|source| ConfigError::Parse {
            path: path.clone(),
            source,
        })?;

        Self::from_raw(raw, &path)
    }

    fn from_raw(raw: RawConfig, path: &Path) -> Result<Self, ConfigError> {
        let provider = raw.provider.ok_or_else(|| ConfigError::Missing {
            path: path.to_path_buf(),
            key: "provider",
        })?;
        let api_key = provider.api_key.ok_or_else(|| ConfigError::Missing {
            path: path.to_path_buf(),
            key: "provider.api_key",
        })?;
        if api_key.trim().is_empty() {
            return Err(invalid(
                path,
                "provider.api_key",
                "must be a non-empty string",
            ));
        }

        let base_url = normalize_base_url(
            path,
            provider.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL),
        )?;
        let model = normalized_required(
            path,
            "provider.model",
            provider.model.as_deref().unwrap_or(DEFAULT_MODEL),
        )?;
        let api_type = parse_api_type(
            path,
            provider.api_type.as_deref().unwrap_or(DEFAULT_API_TYPE),
        )?;
        let reasoning_effort = parse_reasoning_effort(path, provider.reasoning_effort.as_deref())?;
        let level = normalized_required(
            path,
            "logging.level",
            raw.logging.level.as_deref().unwrap_or(DEFAULT_LOG_LEVEL),
        )?;
        EnvFilter::try_new(&level)
            .map_err(|error| invalid(path, "logging.level", error.to_string()))?;

        Ok(Self {
            provider: ProviderConfig {
                api_key,
                base_url,
                model,
                api_type,
                reasoning_effort,
            },
            logging: LoggingConfig { level },
        })
    }
}

pub fn default_path() -> Result<PathBuf, ConfigError> {
    dirs::home_dir()
        .map(|home| home.join(".mini-agent").join("config.toml"))
        .ok_or(ConfigError::HomeDirectoryUnavailable)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    provider: Option<RawProvider>,
    #[serde(default)]
    logging: RawLogging,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProvider {
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    api_type: Option<String>,
    reasoning_effort: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLogging {
    level: Option<String>,
}

fn normalized_required(path: &Path, key: &'static str, value: &str) -> Result<String, ConfigError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid(path, key, "must be a non-empty string"));
    }
    Ok(value.to_string())
}

fn parse_api_type(path: &Path, value: &str) -> Result<ApiType, ConfigError> {
    match value.trim() {
        "completions" => Ok(ApiType::Completions),
        "responses" => Ok(ApiType::Responses),
        _ => Err(invalid(
            path,
            "provider.api_type",
            "must be one of: completions, responses",
        )),
    }
}

fn parse_reasoning_effort(
    path: &Path,
    value: Option<&str>,
) -> Result<Option<ReasoningEffort>, ConfigError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    let effort = match value {
        "low" => ReasoningEffort::Low,
        "medium" => ReasoningEffort::Medium,
        "high" => ReasoningEffort::High,
        "xhigh" => ReasoningEffort::XHigh,
        _ => {
            return Err(invalid(
                path,
                "provider.reasoning_effort",
                "must be one of: low, medium, high, xhigh",
            ));
        }
    };
    Ok(Some(effort))
}

fn normalize_base_url(path: &Path, value: &str) -> Result<String, ConfigError> {
    let value = value.trim();
    let parsed =
        Url::parse(value).map_err(|error| invalid(path, "provider.base_url", error.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(invalid(
            path,
            "provider.base_url",
            "must be an absolute HTTP or HTTPS URL",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(invalid(
            path,
            "provider.base_url",
            "must not contain query parameters or fragments",
        ));
    }

    let endpoint_path = parsed.path().trim_end_matches('/');
    if endpoint_path.ends_with("/chat/completions") || endpoint_path.ends_with("/responses") {
        return Err(invalid(
            path,
            "provider.base_url",
            "must not include /chat/completions or /responses endpoint paths",
        ));
    }

    Ok(value.trim_end_matches('/').to_string())
}

fn invalid(path: &Path, key: &'static str, message: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        path: path.to_path_buf(),
        key,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{ApiType, AppConfig, ConfigError, ReasoningEffort};

    fn write_config(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempdir().expect("create temporary directory");
        let path = directory.path().join("config.toml");
        fs::write(&path, contents).expect("write test config");
        (directory, path)
    }

    fn minimal_config() -> &'static str {
        r#"
[provider]
api_key = "test-secret"
"#
    }

    #[test]
    fn loads_required_value_and_defaults() {
        let (_directory, path) = write_config(minimal_config());

        let config = AppConfig::load_from(&path).expect("load config");

        assert_eq!(config.provider.api_key, "test-secret");
        assert_eq!(config.provider.base_url, "https://api.openai.com/v1");
        assert_eq!(config.provider.model, "gpt-5.5");
        assert_eq!(config.provider.api_type, ApiType::Completions);
        assert_eq!(config.provider.reasoning_effort, None);
        assert_eq!(config.logging.level, "info");
    }

    #[test]
    fn loads_custom_provider_and_logging_values() {
        let (_directory, path) = write_config(
            r#"
[provider]
api_key = "test-secret"
base_url = "https://example.test/v1/"
model = "custom-model"
api_type = "responses"
reasoning_effort = "high"

[logging]
level = "info,provider=debug"
"#,
        );

        let config = AppConfig::load_from(&path).expect("load config");

        assert_eq!(config.provider.base_url, "https://example.test/v1");
        assert_eq!(config.provider.model, "custom-model");
        assert_eq!(config.provider.api_type, ApiType::Responses);
        assert_eq!(
            config.provider.reasoning_effort,
            Some(ReasoningEffort::High)
        );
        assert_eq!(config.logging.level, "info,provider=debug");
    }

    #[test]
    fn blank_reasoning_effort_is_none() {
        let (_directory, path) = write_config(
            r#"
[provider]
api_key = "test-secret"
reasoning_effort = "   "
"#,
        );

        let config = AppConfig::load_from(&path).expect("load config");

        assert_eq!(config.provider.reasoning_effort, None);
    }

    #[test]
    fn rejects_missing_api_key_with_path_and_key() {
        let (_directory, path) = write_config("[provider]\n");

        let error = AppConfig::load_from(&path).expect_err("missing api key should fail");
        let message = error.to_string();

        assert!(message.contains(path.to_string_lossy().as_ref()));
        assert!(message.contains("provider.api_key"));
    }

    #[test]
    fn rejects_missing_file_with_path() {
        let directory = tempdir().expect("create temporary directory");
        let path = directory.path().join("missing.toml");

        let error = AppConfig::load_from(&path).expect_err("missing file should fail");

        assert!(matches!(error, ConfigError::MissingFile { .. }));
        assert!(error.to_string().contains(path.to_string_lossy().as_ref()));
        assert!(error.to_string().contains("provider"));
    }

    #[test]
    fn rejects_malformed_toml() {
        let (_directory, path) = write_config("[provider\napi_key = \"secret\"");

        let error = AppConfig::load_from(&path).expect_err("malformed TOML should fail");

        assert!(matches!(error, ConfigError::Parse { .. }));
    }

    #[test]
    fn rejects_unknown_keys() {
        let (_directory, path) = write_config(
            r#"
[provider]
api_key = "test-secret"
unknown = "value"
"#,
        );

        let error = AppConfig::load_from(&path).expect_err("unknown key should fail");

        assert!(matches!(error, ConfigError::Parse { .. }));
    }

    #[test]
    fn rejects_invalid_api_type() {
        let (_directory, path) = write_config(
            r#"
[provider]
api_key = "test-secret"
api_type = "chat"
"#,
        );

        let error = AppConfig::load_from(&path).expect_err("invalid API type should fail");

        assert!(error.to_string().contains("provider.api_type"));
        assert!(error.to_string().contains("completions"));
    }

    #[test]
    fn rejects_invalid_reasoning_effort() {
        let (_directory, path) = write_config(
            r#"
[provider]
api_key = "test-secret"
reasoning_effort = "minimal"
"#,
        );

        let error = AppConfig::load_from(&path).expect_err("invalid reasoning effort should fail");

        assert!(error.to_string().contains("provider.reasoning_effort"));
        assert!(error.to_string().contains("xhigh"));
    }

    #[test]
    fn rejects_invalid_base_url() {
        for base_url in [
            "relative/path",
            "ftp://example.test/v1",
            "https://example.test/v1?key=value",
            "https://example.test/v1#fragment",
            "https://example.test/v1/chat/completions",
            "https://example.test/v1/responses",
        ] {
            let contents =
                format!("[provider]\napi_key = \"test-secret\"\nbase_url = \"{base_url}\"\n");
            let (_directory, path) = write_config(&contents);

            let error = AppConfig::load_from(&path).expect_err("invalid base URL should fail");

            assert!(error.to_string().contains("provider.base_url"));
        }
    }

    #[test]
    fn rejects_invalid_logging_filter() {
        let (_directory, path) = write_config(
            r#"
[provider]
api_key = "test-secret"

[logging]
level = "foo=bar=baz"
"#,
        );

        let error = AppConfig::load_from(&path).expect_err("invalid log filter should fail");

        assert!(error.to_string().contains("logging.level"));
    }

    #[test]
    fn debug_output_redacts_api_key() {
        let (_directory, path) = write_config(minimal_config());
        let config = AppConfig::load_from(&path).expect("load config");

        let debug = format!("{config:?}");

        assert!(!debug.contains("test-secret"));
        assert!(debug.contains("[REDACTED]"));
    }
}
