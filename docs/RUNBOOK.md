# Runbook

This runbook reflects the repository as it exists today: a local Rust CLI application that talks to an OpenAI-compatible API. There is no deployment automation, service manifest, or infrastructure definition checked into this repository.

## Deployment Procedures

Current deployment model: local execution.

### Local startup

1. Ensure Rust is installed.
2. Create `~/.mini-agent/config.toml` with the provider and logging settings.
3. Start the CLI:

```bash
cargo run -p agent-cli
```

### Release build

```bash
cargo build --release -p agent-cli
```

The binary can then be distributed or launched from `target/release/agent-cli` in environments where the required user configuration file is available.

## Monitoring and Alerts

There is no built-in production monitoring or alerting integration in the repository.

Available observability today:

- Terminal stdout for streamed assistant output.
- Terminal stderr for request and stream failures.
- `tracing_subscriber` with the `logging.level` value from `~/.mini-agent/config.toml`.

Example:

The logging level is configured in TOML:

```toml
[logging]
level = "info"
```

If you need operational monitoring later, good next steps would be:

- structured logging output,
- request identifiers,
- latency and error metrics,
- retry counters,
- external health checks for any future hosted wrapper.

## Common Issues and Fixes

### Configuration file is missing

Cause: `~/.mini-agent/config.toml` does not exist or is not readable.

Fix:

- create `~/.mini-agent/config.toml` using the configuration example in the README.

### `provider.api_key is missing`

Cause: the required API key is missing or blank.

Fix:

- set a non-empty `api_key` under `[provider]`.

### `provider.api_type is invalid`

Cause: unsupported API type was configured.

Fix:

- set `api_type = "completions"`, or
- set `api_type = "responses"`.

### `provider.reasoning_effort is invalid`

Cause: invalid reasoning effort value.

Fix:

- remove the key, or
- set one of `low`, `medium`, `high`, or `xhigh`.

### Provider API errors such as 401 or 500

Cause: invalid credentials, incompatible base URL, or upstream provider failure.

Fix:

- verify `provider.api_key`,
- verify `provider.base_url`,
- verify the configured endpoint supports the selected `provider.api_type`,
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
- verify the corresponding `~/.mini-agent/config.toml` for the selected binary version,
- rerun smoke checks against the provider endpoint.

## Operational Gaps

The following items are not yet represented in this repository and should be documented later if introduced:

- server deployment targets,
- CI/CD workflows,
- secrets management,
- uptime monitoring,
- alert routing,
- automated rollback.
