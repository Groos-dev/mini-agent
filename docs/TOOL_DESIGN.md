# Tool Calling Design

## Purpose

This document defines the first local tool-calling capability for `mini-agent`.
It lets a model request a declared function, executes that function locally, sends
the result back to the model, and continues until the model produces a final
assistant response.

The initial built-in tool is `shell`, registered by default in the CLI.

## Scope

Implemented in this change:

- Provider-neutral function specifications, calls, results, and stream events.
- A registry for locally executable tools.
- A bounded agent loop that records the assistant call and every tool result in
  the conversation history.
- Function calling for OpenAI-compatible Chat Completions and Responses APIs.
- The default `shell` tool with workspace-directory validation, a per-command
  timeout, bounded captured output, and a sanitized child environment that
  preserves Rust toolchain locations.

Not implemented:

- MCP discovery, transport, and execution.
- Provider-hosted tools such as web search and code interpreter.
- Parallel local tool execution.
- User approval prompts for individual calls.
- OS-level filesystem, network, or process sandboxing.

Each registered tool in this version has a local `ToolExecutor`.

## Architecture

```text
CLI -> Agent -> Provider -> model stream
                  ^             |
                  |             v
             Tool registry <- function call
                  |
                  v
             local executor
```

1. The CLI builds a `ToolRegistry` with the default shell tool.
2. `Agent` sends the conversation and registered `ToolSpec` values to a
   `Provider`.
3. A provider translates its stream into `ChatEvent` values. Text is emitted as
   deltas; complete calls are emitted as `ToolCallDone`.
4. The agent records the complete assistant response, including any text and
   calls. It executes calls sequentially and appends one tool-result message per
   call.
5. The agent requests the next model turn with the expanded history. A final
   response commits the entire working history atomically to the session.

If a provider stream fails or the tool-round limit is reached, the working
history is discarded. Tool failures and invalid arguments are instead converted
to structured tool results so the model can recover in the next turn.

## Core Contracts

`provider::ToolSpec` is the function schema exposed to a model. A `ToolCall`
contains the provider call identifier, function name, and JSON arguments. The
identifier is preserved unchanged so it can correlate a tool result in either
provider protocol.

`agent_core::tool::ToolExecutor` owns local execution. It receives parsed JSON
and a `ToolExecutionContext`, then returns a serializable `ToolResult`. The
registry rejects duplicate tool names and returns a normal tool failure for an
unknown or invalid call.

`AgentRunOptions::max_tool_rounds` defaults to 8. One round is one model
response that contains one or more tool calls. Calls within a round run in the
order supplied by the provider. This caps recursive behavior and makes command
effects deterministic.

## Provider Mapping

### Chat Completions

- Tool schemas use `{ "type": "function", "function": ... }`.
- Assistant call history is sent in `assistant.tool_calls`.
- Tool results use a `tool` message with `tool_call_id` and serialized content.
- Streaming `delta.tool_calls` fragments are accumulated by index and emitted as
  a complete call when `finish_reason` is `tool_calls`.

### Responses

- Tool schemas use top-level function fields: `type`, `name`, `description`, and
  `parameters`.
- Conversation text is sent as input message items.
- Assistant calls are preserved as `function_call` input items.
- Tool results are sent as `function_call_output` input items using the same
  `call_id`.
- `response.function_call_arguments.delta` is translated to a generic call
  delta. `response.output_item.done` yields the complete function call used by
  the agent loop.

## Shell Tool Policy

The CLI always registers `shell` for this feature branch and requires explicit
approval before executing each tool call.

The tool accepts:

```json
{
  "command": "cargo test -p agent-core",
  "cwd": "crates/agent-core",
  "timeout_ms": 30000
}
```

`cwd` is resolved relative to the invocation context and must canonicalize under
the configured workspace root. Commands run without stdin, without a login
shell, with a 1-120 second timeout selected by `timeout_ms`, and with at most 64 KiB captured separately
from stdout and stderr. Capturing is bounded while the process runs. The child
environment is cleared before setting a minimal `PATH`, `HOME`, `LANG`, and
`TERM`, `CARGO_HOME`, and `RUSTUP_HOME`; this prevents the API key and other
parent environment variables from being inherited while keeping standard Rust
development commands usable. On Unix, each command has a separate process group
that is terminated as a unit when the timeout expires.

These controls do not make arbitrary shell execution a security sandbox. The
CLI asks for explicit user approval before each tool call, but an approved
shell command can still attempt to access absolute paths, create child
processes, or use the network. Running untrusted prompts or repositories
requires an external sandbox with filesystem and network policies.

## Events And CLI Output

`AgentEvent` is the public progress stream:

- `TextChunk`
- `ToolCallStarted`
- `ToolCallFinished`
- `ToolCallFailed`
- `TurnFinished`

The CLI prints text as it arrives and emits concise local status lines for tool
execution. It does not print tool output by default, which may contain large or
sensitive data.

## Verification

The implementation must cover:

- request serialization and streamed tool-call parsing for both APIs;
- a complete two-turn agent loop with successful and failed calls;
- history preservation when an assistant message includes text and calls;
- duplicate/unknown tools and invalid JSON arguments;
- maximum tool rounds;
- shell input validation, directory escape rejection, timeout, bounded output,
  and sanitized environment behavior;
- CLI registration of the default shell tool.
