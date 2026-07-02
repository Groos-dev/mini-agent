use std::{
    io::{self, BufRead, Write},
    str::FromStr,
};

use agent_core::agent::Agent;
use futures::StreamExt;
use provider::{ApiType, ChatOptions, OpenAIProvider, ReasoningEffort};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    init_tracing();
    let config = load_config();

    let session_options = SessionOptions::from_config(&config);
    let provider = OpenAIProvider::new(
        config.api_key,
        config.base_url,
        config.model,
        config.api_type,
    );
    let mut agent = Agent::new(Box::new(provider));

    print_welcome();

    let stdin = io::stdin();
    let mut reader = stdin.lock();

    loop {
        print!("\nYou> ");
        io::stdout().flush()?;

        let mut bytes = Vec::new();
        if reader.read_until(b'\n', &mut bytes)? == 0 {
            println!();
            break;
        }

        let input = String::from_utf8_lossy(&bytes);
        let input = input.trim();
        if input.is_empty() {
            continue;
        }
        if input == "exit" || input == "quit" {
            break;
        }

        let options = session_options.chat_options();
        let mut stream = match agent.chat_stream(input, options).await {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("\nRequest failed: {err}");
                continue;
            }
        };

        print!("Assistant> ");
        io::stdout().flush()?;

        let mut full = String::new();
        let mut stream_failed = false;

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(piece) => {
                    print!("{piece}");
                    io::stdout().flush()?;
                    full.push_str(&piece);
                }
                Err(err) => {
                    stream_failed = true;
                    eprintln!("\nStream failed: {err}");
                    break;
                }
            }
        }
        println!();

        if stream_failed {
            agent.discard_pending_user_message();
            continue;
        }

        if !full.is_empty() {
            agent.push_assistant_message(full);
        }
    }

    println!("Bye.");
    Ok(())
}

struct Config {
    api_key: String,
    base_url: String,
    model: String,
    api_type: ApiType,
    reasoning_effort: Option<ReasoningEffort>,
}

struct SessionOptions {
    reasoning_effort: Option<ReasoningEffort>,
}

impl SessionOptions {
    fn from_config(config: &Config) -> Self {
        Self {
            reasoning_effort: config.reasoning_effort,
        }
    }

    fn chat_options(&self) -> ChatOptions {
        ChatOptions {
            reasoning_effort: self.reasoning_effort,
        }
    }
}

fn load_config() -> Config {
    load_config_from_env(|key| std::env::var(key).ok())
}

fn load_config_from_env<F>(get: F) -> Config
where
    F: Fn(&str) -> Option<String>,
{
    let api_key = get("OPENAI_API_KEY").expect("OPENAI_API_KEY is not set");
    let base_url = get("OPENAI_BASE_URL").unwrap_or_else(|| "https://api.openai.com/v1".into());
    let model = get("OPENAI_MODEL").unwrap_or_else(|| "gpt-5.5".to_string());
    let api_type = get("OPENAI_API_TYPE")
        .unwrap_or_else(|| "completions".to_string())
        .parse()
        .expect("OPENAI_API_TYPE must be one of: responses, completions");
    let reasoning_effort = get("OPENAI_REASONING_EFFORT")
        .filter(|v| !v.trim().is_empty())
        .map(|v| ReasoningEffort::from_str(v.trim()))
        .transpose()
        .expect("OPENAI_REASONING_EFFORT must be one of: low, medium, high, xhigh");

    Config {
        api_key,
        base_url,
        model,
        api_type,
        reasoning_effort,
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
}

fn print_welcome() {
    println!("mini-agent");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config_from(values: &[(&str, &str)]) -> Config {
        let env: HashMap<&str, &str> = values.iter().copied().collect();
        load_config_from_env(|key| env.get(key).map(|value| (*value).to_string()))
    }

    #[test]
    fn load_config_uses_required_api_key() {
        let config = config_from(&[("OPENAI_API_KEY", "key")]);

        assert_eq!(config.api_key, "key");
    }

    #[test]
    fn load_config_uses_defaults_for_optional_values() {
        let config = config_from(&[("OPENAI_API_KEY", "key")]);

        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.model, "gpt-5.5");
        assert_eq!(config.api_type, ApiType::Completions);
        assert_eq!(config.reasoning_effort, None);
    }

    #[test]
    fn load_config_reads_custom_provider_values() {
        let config = config_from(&[
            ("OPENAI_API_KEY", "key"),
            ("OPENAI_BASE_URL", "https://example.test/v1"),
            ("OPENAI_MODEL", "custom-model"),
            ("OPENAI_API_TYPE", "responses"),
        ]);

        assert_eq!(config.base_url, "https://example.test/v1");
        assert_eq!(config.model, "custom-model");
        assert_eq!(config.api_type, ApiType::Responses);
    }

    #[test]
    fn load_config_parses_reasoning_effort() {
        let config = config_from(&[
            ("OPENAI_API_KEY", "key"),
            ("OPENAI_REASONING_EFFORT", "high"),
        ]);

        assert_eq!(config.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn load_config_treats_blank_reasoning_effort_as_none() {
        let config = config_from(&[
            ("OPENAI_API_KEY", "key"),
            ("OPENAI_REASONING_EFFORT", "   "),
        ]);

        assert_eq!(config.reasoning_effort, None);
    }

    #[test]
    #[should_panic(expected = "OPENAI_API_KEY is not set")]
    fn load_config_panics_without_api_key() {
        let _ = config_from(&[]);
    }

    #[test]
    #[should_panic(expected = "OPENAI_REASONING_EFFORT must be one of")]
    fn load_config_panics_for_invalid_reasoning_effort() {
        let _ = config_from(&[
            ("OPENAI_API_KEY", "key"),
            ("OPENAI_REASONING_EFFORT", "minimal"),
        ]);
    }

    #[test]
    #[should_panic(expected = "OPENAI_API_TYPE must be one of")]
    fn load_config_panics_for_invalid_api_type() {
        let _ = config_from(&[("OPENAI_API_KEY", "key"), ("OPENAI_API_TYPE", "chat")]);
    }

    #[test]
    fn session_options_from_config_copies_reasoning_effort() {
        let config = Config {
            api_key: "key".to_string(),
            base_url: "https://example.test/v1".to_string(),
            model: "model".to_string(),
            api_type: ApiType::Completions,
            reasoning_effort: Some(ReasoningEffort::Medium),
        };

        let options = SessionOptions::from_config(&config);

        assert_eq!(options.reasoning_effort, Some(ReasoningEffort::Medium));
    }

    #[test]
    fn chat_options_returns_provider_chat_options() {
        let options = SessionOptions {
            reasoning_effort: Some(ReasoningEffort::XHigh),
        };

        assert_eq!(
            options.chat_options().reasoning_effort,
            Some(ReasoningEffort::XHigh)
        );
    }
}
