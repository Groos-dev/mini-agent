# mini-agent

`mini-agent` is a small Rust workspace for experimenting with a streaming, multi-turn CLI agent. It separates the interactive command-line loop, conversation state management, and provider integration into independent crates.

## Workspace Layout

| Path | Crate | Purpose |
| --- | --- | --- |
| `crates/agent-cli` | `agent-cli` | Interactive terminal application that loads configuration, reads user input, streams assistant output, and maintains a session loop. |
| `crates/agent-config` | `agent-config` | User-scoped TOML configuration discovery, parsing, defaulting, validation, and secret redaction. |
| `crates/agent-core` | `agent-core` | Provider-agnostic agent state, message history, and chat request orchestration. |
| `crates/provider` | `provider` | Provider trait and OpenAI-compatible streaming implementations for Chat Completions and Responses APIs. |

## Features

- Streaming assistant responses in the terminal.
- Multi-turn conversation history within a CLI session.
- OpenAI-compatible provider abstraction.
- Support for both `/chat/completions` and `/responses` streaming endpoints.
- Optional `reasoning_effort` configuration for compatible models.
- Unit tests for configuration parsing, agent state handling, and streaming parsers.

## Requirements

- Rust toolchain with edition 2024 support.
- Network access to an OpenAI-compatible API endpoint.
- An API key for the configured provider.

## Configuration

The CLI loads configuration from `~/.mini-agent/config.toml`. It does not read
`.env` files or use process environment variables as configuration overrides.

The configuration file must contain a non-empty API key:

```toml
[provider.openai]
api_key = "your-api-key"
base_url = "https://api.openai.com/v1"
model = "gpt-5.5"
api_type = "completions"
reasoning_effort = "medium"

[logging]
level = "info"
```

| TOML key | Required | Default | Description |
| --- | --- | --- | --- |
| `provider.openai.api_key` | Yes | None | Bearer token used for provider requests. |
| `provider.openai.base_url` | No | `https://api.openai.com/v1` | Base URL for the OpenAI-compatible API. Do not include endpoint paths such as `/chat/completions`. |
| `provider.openai.model` | No | `gpt-5.5` | Model name sent to the provider. |
| `provider.openai.api_type` | No | `completions` | Streaming API variant. Supported values: `completions`, `responses`. |
| `provider.openai.reasoning_effort` | No | None | Optional reasoning effort. Supported values: `low`, `medium`, `high`, `xhigh`. |
| `logging.level` | No | `info` | `tracing-subscriber` filter directives. |

## Usage

Run the CLI from the workspace root:

```bash
cargo run -p agent-cli
```

Inside the interactive session:

- Type a prompt and press Enter to stream a response.
- Type `exit` or `quit` to end the session.
- Empty lines are ignored.

## Development

Common commands:

```bash
cargo fmt
cargo test
cargo run -p agent-cli
```

Run tests for a single crate:

```bash
cargo test -p provider
cargo test -p agent-core
cargo test -p agent-cli
```

## Architecture

The workspace keeps provider-specific behavior out of the core agent loop:

1. `agent-cli` loads the user TOML configuration and reads terminal input.
2. `agent-core` builds provider-neutral chat requests from session history.
3. `provider` sends requests to the configured OpenAI-compatible endpoint and returns a stream of text chunks.
4. `agent-cli` prints streamed chunks and commits the assistant message to history only after the stream completes successfully.

This split keeps the CLI thin while making it possible to add more provider implementations behind the shared `Provider` trait.

## Documentation

- [Contributing guide](docs/CONTRIB.md)
- [Runbook](docs/RUNBOOK.md)
