use std::{
    io::{self, BufRead, Write},
    str::FromStr,
};

use agent_core::{
    agent::{Agent, AgentEvent, AgentRunOptions},
    tool::{
        ToolRegistry,
        shell::{ShellTool, ShellToolConfig},
    },
};
use agent_protocol::{ModelOptions, ReasoningEffort};
use futures::StreamExt;
use openai_provider::{ApiType, OpenAIProvider};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    init_tracing();
    let config = load_config();

    let session_options = SessionOptions::from_config(&config);
    let provider = OpenAIProvider::new(
        config.api_key.clone(),
        config.base_url.clone(),
        config.model.clone(),
        config.api_type,
    );
    let tool_registry = build_tool_registry()?;
    let mut agent = Agent::new(Box::new(provider), tool_registry);

    print_welcome();

    let stdin = io::stdin();
    let mut stdin = stdin.lock();

    loop {
        print!("\nYou> ");
        io::stdout().flush()?;

        let Some(input) = read_input_line(&mut stdin)? else {
            println!();
            break;
        };

        let input = input.trim();
        if input.is_empty() {
            continue;
        }
        if input == "exit" || input == "quit" {
            break;
        }

        let options = session_options.agent_run_options();
        let mut stream = match agent.run_stream(input, options).await {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("\nRequest failed: {err}");
                continue;
            }
        };

        print!("Assistant> ");
        io::stdout().flush()?;

        while let Some(event) = stream.next().await {
            match event {
                Ok(AgentEvent::TextChunk(piece)) => {
                    print!("{piece}");
                    io::stdout().flush()?;
                }
                Ok(AgentEvent::ToolCallStarted {
                    name, arguments, ..
                }) => {
                    println!("\n[tool:{name}] started {arguments}");
                    io::stdout().flush()?;
                }
                Ok(AgentEvent::ToolCallFinished { name, result, .. }) => {
                    println!("\n[tool:{name}] finished success={}", result.success);
                    io::stdout().flush()?;
                }
                Ok(AgentEvent::ToolCallFailed { name, error, .. }) => {
                    println!("\n[tool:{name}] failed {error}");
                    io::stdout().flush()?;
                }
                Ok(AgentEvent::TurnFinished) => {}
                Err(err) => {
                    eprintln!("\nStream failed: {err}");
                    break;
                }
            }
        }
        println!();
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

    fn model_options(&self) -> ModelOptions {
        ModelOptions {
            reasoning_effort: self.reasoning_effort,
        }
    }

    fn agent_run_options(&self) -> AgentRunOptions {
        AgentRunOptions::from_model_options(self.model_options())
    }
}

fn load_config() -> Config {
    load_config_from_env(|key| std::env::var(key).ok())
}

fn read_input_line(input: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    if input.read_until(b'\n', &mut bytes)? == 0 {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn load_config_from_env<F>(get: F) -> Config
where
    F: Fn(&str) -> Option<String>,
{
    let api_key = get("OPENAI_API_KEY").expect("OPENAI_API_KEY is not set");
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        panic!("OPENAI_API_KEY must not be empty");
    }
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

fn build_tool_registry() -> anyhow::Result<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ShellTool::new(ShellToolConfig::default())))?;
    Ok(registry)
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
    use std::{collections::HashMap, io::Cursor};

    use super::*;

    #[test]
    fn read_input_line_replaces_invalid_utf8_without_failing() {
        let mut input = Cursor::new(b"hello\xFF\n".to_vec());

        assert_eq!(
            read_input_line(&mut input).unwrap(),
            Some("hello\u{FFFD}\n".to_string())
        );
        assert_eq!(read_input_line(&mut input).unwrap(), None);
    }

    fn config_from(values: &[(&str, &str)]) -> Config {
        let env: HashMap<&str, &str> = values.iter().copied().collect();
        load_config_from_env(|key| env.get(key).map(|value| (*value).to_string()))
    }

    #[test]
    fn load_config_uses_required_api_key_and_defaults() {
        let config = config_from(&[("OPENAI_API_KEY", "key")]);

        assert_eq!(config.api_key, "key");
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.model, "gpt-5.5");
        assert_eq!(config.api_type, ApiType::Completions);
        assert_eq!(config.reasoning_effort, None);
    }

    #[test]
    fn load_config_reads_provider_settings() {
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
    #[should_panic(expected = "OPENAI_API_KEY must not be empty")]
    fn load_config_panics_for_blank_api_key() {
        let _ = config_from(&[("OPENAI_API_KEY", "  ")]);
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
    fn build_tool_registry_registers_shell_tool() {
        assert_eq!(build_tool_registry().unwrap().specs()[0].name, "shell");
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
    fn model_options_preserve_reasoning_effort() {
        let options = SessionOptions {
            reasoning_effort: Some(ReasoningEffort::XHigh),
        };

        assert_eq!(
            options.model_options().reasoning_effort,
            Some(ReasoningEffort::XHigh)
        );
    }
}
