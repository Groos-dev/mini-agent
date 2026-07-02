# Contributing

This repository is a Rust workspace. The current source of truth for local development is the workspace `Cargo.toml` files and the environment-variable loading logic in `crates/agent-cli/src/main.rs`.

## Development Workflow

1. Install a recent Rust toolchain with edition 2024 support.
2. Create a local `.env` file in the repository root.
3. Set the required provider variables.
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

## Environment Setup

The CLI reads configuration from process environment variables and supports `.env` through `dotenvy`.

| Variable | Required | Default | Purpose | Format |
| --- | --- | --- | --- | --- |
| `OPENAI_API_KEY` | Yes | None | Authenticates requests to the configured provider. | Non-empty string |
| `OPENAI_BASE_URL` | No | `https://api.openai.com/v1` | Overrides the provider base URL. | Absolute URL without endpoint suffix |
| `OPENAI_MODEL` | No | `gpt-5.5` | Selects the target model. | Model id string |
| `OPENAI_API_TYPE` | No | `completions` | Selects which streaming API contract to use. | `completions` or `responses` |
| `OPENAI_REASONING_EFFORT` | No | Empty | Enables provider reasoning effort when supported. | `low`, `medium`, `high`, or `xhigh` |

Example:

```env
OPENAI_API_KEY=your-api-key
OPENAI_BASE_URL=https://api.openai.com/v1
OPENAI_MODEL=gpt-5.5
OPENAI_API_TYPE=completions
OPENAI_REASONING_EFFORT=medium
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
cargo test -p agent-cli
```

### Manual verification

```bash
cargo run -p agent-cli
```

Recommended manual checks:

- Verify startup succeeds when all required environment variables are present.
- Verify startup fails clearly when `OPENAI_API_KEY` is missing.
- Verify both `OPENAI_API_TYPE=completions` and `OPENAI_API_TYPE=responses` work against a compatible endpoint.
- Verify failed streaming responses do not append incomplete assistant messages to session history.

## Notes

The original documentation sync template referenced `package.json` and `.env.example`, but those files do not exist in this repository today. This document is based on the actual Rust workspace manifests and source code instead.
