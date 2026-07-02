# mini-agent

`mini-agent` is a small Rust workspace for experimenting with a streaming, multi-turn CLI agent. It separates the interactive command-line loop, conversation state management, and provider integration into independent crates.

## Workspace Layout

| Path | Crate | Purpose |
| --- | --- | --- |
| `crates/agent-cli` | `agent-cli` | Interactive terminal application that loads configuration, reads user input, streams assistant output, and maintains a session loop. |
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

The CLI loads environment variables directly and also supports a local `.env` file through `dotenvy`.

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `OPENAI_API_KEY` | Yes | None | Bearer token used for provider requests. |
| `OPENAI_BASE_URL` | No | `https://api.openai.com/v1` | Base URL for the OpenAI-compatible API. Do not include endpoint paths such as `/chat/completions`. |
| `OPENAI_MODEL` | No | `gpt-5.5` | Model name sent to the provider. |
| `OPENAI_API_TYPE` | No | `completions` | Streaming API variant. Supported values: `completions`, `responses`. |
| `OPENAI_REASONING_EFFORT` | No | Empty | Optional reasoning effort. Supported values: `low`, `medium`, `high`, `xhigh`. Blank values are ignored. |

Example `.env`:

```env
OPENAI_API_KEY=your-api-key
OPENAI_BASE_URL=https://api.openai.com/v1
OPENAI_MODEL=gpt-5.5
OPENAI_API_TYPE=completions
OPENAI_REASONING_EFFORT=medium
```

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

1. `agent-cli` reads terminal input and environment configuration.
2. `agent-core` builds provider-neutral chat requests from session history.
3. `provider` sends requests to the configured OpenAI-compatible endpoint and returns a stream of text chunks.
4. `agent-cli` prints streamed chunks and commits the assistant message to history only after the stream completes successfully.

This split keeps the CLI thin while making it possible to add more provider implementations behind the shared `Provider` trait.

## Documentation

- [Contributing guide](docs/CONTRIB.md)
- [Runbook](docs/RUNBOOK.md)
