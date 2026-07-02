# Runbook

This runbook reflects the repository as it exists today: a local Rust CLI application that talks to an OpenAI-compatible API. There is no deployment automation, service manifest, or infrastructure definition checked into this repository.

## Deployment Procedures

Current deployment model: local execution.

### Local startup

1. Ensure Rust is installed.
2. Configure environment variables or create a `.env` file.
3. Start the CLI:

```bash
cargo run -p agent-cli
```

### Release build

```bash
cargo build --release -p agent-cli
```

The binary can then be distributed or launched from `target/release/agent-cli` in environments where the required variables are available.

## Monitoring and Alerts

There is no built-in production monitoring or alerting integration in the repository.

Available observability today:

- Terminal stdout for streamed assistant output.
- Terminal stderr for request and stream failures.
- `tracing_subscriber` with `RUST_LOG` / `RUST_LOG_STYLE` environment-based filtering.

Example:

```bash
RUST_LOG=info cargo run -p agent-cli
```

If you need operational monitoring later, good next steps would be:

- structured logging output,
- request identifiers,
- latency and error metrics,
- retry counters,
- external health checks for any future hosted wrapper.

## Common Issues and Fixes

### `OPENAI_API_KEY is not set`

Cause: required API key is missing.

Fix:

- export `OPENAI_API_KEY`, or
- add it to a local `.env` file in the repository root.

### `OPENAI_API_TYPE must be one of: responses, completions`

Cause: unsupported API type was configured.

Fix:

- set `OPENAI_API_TYPE=completions`, or
- set `OPENAI_API_TYPE=responses`.

### `OPENAI_REASONING_EFFORT must be one of: low, medium, high, xhigh`

Cause: invalid reasoning effort value.

Fix:

- remove the variable, or
- set one of the supported values.

### Provider API errors such as 401 or 500

Cause: invalid credentials, incompatible base URL, or upstream provider failure.

Fix:

- verify `OPENAI_API_KEY`,
- verify `OPENAI_BASE_URL`,
- verify the configured endpoint supports the selected `OPENAI_API_TYPE`,
- retry after upstream service recovery.

### Streaming starts but ends with `Stream failed`

Cause: network interruption or malformed/incompatible streaming response.

Fix:

- verify network connectivity,
- confirm the endpoint returns SSE-style `data:` frames,
- confirm the selected API type matches the provider response contract.

Operational note: when a stream fails, the CLI discards the pending user message instead of committing a partial assistant turn.

## Rollback Procedures

There is no formal deployment pipeline in this repository, so rollback currently means reverting code or running a previously built binary.

### Source rollback

```bash
git log --oneline
git checkout <known-good-commit>
cargo test
cargo run -p agent-cli
```

### Binary rollback

- keep a previously validated release binary,
- restore the previous environment configuration if it changed,
- rerun smoke checks against the provider endpoint.

## Operational Gaps

The following items are not yet represented in this repository and should be documented later if introduced:

- server deployment targets,
- CI/CD workflows,
- secrets management,
- uptime monitoring,
- alert routing,
- automated rollback.
