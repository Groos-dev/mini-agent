use agent_config::{
    ApiType as ConfigApiType, AppConfig, ProviderConfig, ReasoningEffort as ConfigReasoningEffort,
};
use agent_core::{
    agent::{Agent, AgentRunOptions},
    tool::{
        ToolRegistry,
        shell::{ShellTool, ShellToolConfig},
    },
};
use agent_protocol::{ModelOptions, ReasoningEffort};
use agent_tui::{TuiMetadata, run};
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
    let agent = Agent::new(Box::new(provider), tool_registry);

    run(
        agent,
        session_options.agent_run_options(),
        TuiMetadata::new(
            provider_config.model.clone(),
            api_type_label(provider_config.api_type),
        ),
    )
    .await?;
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

fn api_type_label(api_type: ConfigApiType) -> &'static str {
    match api_type {
        ConfigApiType::Completions => "completions",
        ConfigApiType::Responses => "responses",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_tool_registry_registers_shell_tool() {
        assert_eq!(build_tool_registry().unwrap().specs()[0].name, "shell");
    }

    #[test]
    fn maps_config_values_to_protocol_values() {
        assert_eq!(map_api_type(ConfigApiType::Responses), ApiType::Responses);
        assert_eq!(api_type_label(ConfigApiType::Completions), "completions");
        assert_eq!(api_type_label(ConfigApiType::Responses), "responses");
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
