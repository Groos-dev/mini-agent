use std::io::{self, BufRead, Write};

use agent_config::{
    ApiType as ConfigApiType, AppConfig, ProviderConfig, ReasoningEffort as ConfigReasoningEffort,
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
    let config = AppConfig::load_default()?;
    init_tracing(&config.logging.level)?;

    let provider_config = &config.provider;
    let session_options = SessionOptions::from_config(provider_config);
    let provider = OpenAIProvider::new(
        provider_config.api_key.clone(),
        provider_config.base_url.clone(),
        provider_config.model.clone(),
        map_api_type(provider_config.api_type),
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

struct SessionOptions {
    reasoning_effort: Option<ReasoningEffort>,
}

impl SessionOptions {
    fn from_config(config: &ProviderConfig) -> Self {
        Self {
            reasoning_effort: config.reasoning_effort.map(map_reasoning_effort),
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

fn map_api_type(api_type: ConfigApiType) -> ApiType {
    match api_type {
        ConfigApiType::Completions => ApiType::Completions,
        ConfigApiType::Responses => ApiType::Responses,
    }
}

fn map_reasoning_effort(effort: ConfigReasoningEffort) -> ReasoningEffort {
    match effort {
        ConfigReasoningEffort::Low => ReasoningEffort::Low,
        ConfigReasoningEffort::Medium => ReasoningEffort::Medium,
        ConfigReasoningEffort::High => ReasoningEffort::High,
        ConfigReasoningEffort::XHigh => ReasoningEffort::XHigh,
    }
}

fn read_input_line(input: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    if input.read_until(b'\n', &mut bytes)? == 0 {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn build_tool_registry() -> anyhow::Result<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ShellTool::new(ShellToolConfig::default())))?;
    Ok(registry)
}

fn init_tracing(level: &str) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_new(level)?)
        .init();
    Ok(())
}

fn print_welcome() {
    println!("mini-agent");
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

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

    #[test]
    fn build_tool_registry_registers_shell_tool() {
        assert_eq!(build_tool_registry().unwrap().specs()[0].name, "shell");
    }

    #[test]
    fn maps_config_values_to_protocol_values() {
        assert_eq!(map_api_type(ConfigApiType::Responses), ApiType::Responses);
        assert_eq!(
            map_reasoning_effort(ConfigReasoningEffort::Medium),
            ReasoningEffort::Medium
        );
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
