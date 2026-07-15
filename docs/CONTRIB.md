# Contributing

This repository is a Rust workspace. The current source of truth for local development is the workspace `Cargo.toml` files and the configuration module in `crates/agent-config`.

## Development Workflow

1. Install a recent Rust toolchain with edition 2024 support.
2. Create `~/.mini-agent/config.toml`.
3. Set the required provider values in the TOML file.
4. Run formatting and tests before submitting changes.
5. Run the CLI manually to verify interactive behavior when touching session or streaming logic.

## Available Commands

Because this repository is a Cargo workspace, the main development commands are:

| Command | Purpose |
| --- | --- |
| `cargo fmt` | Format all Rust code in the workspace. |
| `cargo test` | Run all unit tests across all crates. |
| `cargo run -p agent-cli` | Start the interactive CLI agent. |
| `cargo test -p provider` | Run provider parsing and transport tests only. |
| `cargo test -p agent-core` | Run agent state and history tests only. |
| `cargo test -p agent-cli` | Run CLI configuration and session option tests only. |

## Configuration Setup

The CLI reads configuration only from `~/.mini-agent/config.toml`. This task does not add a configuration initialization command; create the file directly.

| TOML key | Required | Default | Purpose | Format |
| --- | --- | --- | --- | --- |
| `provider.api_key` | Yes | None | Authenticates requests to the configured provider. | Non-empty string |
| `provider.base_url` | No | `https://api.openai.com/v1` | Overrides the provider base URL. | Absolute HTTP(S) URL without endpoint suffix |
| `provider.model` | No | `gpt-5.5` | Selects the target model. | Model id string |
| `provider.api_type` | No | `completions` | Selects which streaming API contract to use. | `completions` or `responses` |
| `provider.reasoning_effort` | No | None | Enables provider reasoning effort when supported. | `low`, `medium`, `high`, or `xhigh` |
| `logging.level` | No | `info` | Controls tracing output. | `EnvFilter` directives |

Example:

```toml
[provider]
api_key = "your-api-key"
base_url = "https://api.openai.com/v1"
model = "gpt-5.5"
api_type = "completions"
reasoning_effort = "medium"

[logging]
level = "info"
```

## Testing Procedures

### Full workspace

```bash
cargo test
```

### Focused testing

```bash
cargo test -p provider
cargo test -p agent-core
cargo test -p agent-config
cargo test -p agent-cli
```

### Manual verification

```bash
cargo run -p agent-cli
```

Recommended manual checks:

- Verify startup succeeds with a valid `~/.mini-agent/config.toml`.
- Verify startup fails clearly when the configuration file or `provider.api_key` is missing.
- Verify both `provider.api_type = "completions"` and `provider.api_type = "responses"` work against a compatible endpoint.
- Verify failed streaming responses do not append incomplete assistant messages to session history.

## Notes

The original documentation sync template referenced `package.json` and `.env.example`, but those files do not exist in this repository today. This document is based on the actual Rust workspace manifests and source code instead.
