# agent-tui Design

## Goal

Add a full-screen terminal user interface for `mini-agent`, with an
OpenCode-style workbench interaction model rather than a line-oriented
terminal transcript. The first version must support a scrollable conversation,
multi-line input, streamed assistant output, tool status updates, and clean
exit behavior.

## Scope and Non-Goals

### In scope

- A new workspace crate at `crates/agent-tui`.
- Full-screen alternate-screen rendering using `ratatui` and `crossterm`.
- A top status bar showing the application, model, API type, and run state.
- A scrollable, wrapped message viewport for user, assistant, tool, and error
  entries.
- A multi-line composer with cursor movement and editing.
- Incremental rendering of `AgentEvent::TextChunk` values.
- Tool-call started, completed, and failed status entries.
- Automatic follow mode, manual scrolling, and an unread indicator when the
  user is browsing older messages.
- Terminal restoration on normal exit and panic/unwind paths.
- Unit tests for state transitions, input editing, scrolling, and event
  projection.

### Out of scope

- Changes to the provider protocol or `agent-core` event contract.
- Conversation persistence across process restarts.
- Multiple sessions, model switching, or configuration editing inside the TUI.
- Mouse interaction, markdown rendering, syntax highlighting, and image
  display in the first version.

## Architecture

`agent-cli` remains responsible for loading configuration and constructing the
provider, tool registry, and `Agent`. It passes those runtime dependencies to
`agent-tui`, whose public runner owns the terminal session and event loop.

`agent-tui` is split into three responsibilities:

1. `AppState` stores the message list, composer buffer and cursor, scroll
   position, request state, and status metadata. It is deterministic and does
   not perform terminal I/O.
2. A Tokio worker owns the `Agent`, receives submitted prompts through a
   channel, and forwards stream events through a second channel. The event
   loop reads terminal events and worker events together, so terminal input
   remains responsive while a request is active and cannot submit another
   prompt.
3. The renderer converts `AppState` into ratatui widgets. It calculates the
   wrapped message height, preserves the user's manual scroll position, and
   renders the composer and footer without changing layout dimensions.

The runner accepts an `Agent` and session run options, so the existing provider
and agent history behavior is preserved. A submitted prompt is added to the
visible state immediately. Text chunks append to the current assistant entry;
tool events append/update visible tool status; stream failures become an error
entry and return the UI to idle without committing partial assistant output in
the core agent history.

## Layout and Interaction

The screen is rendered as:

```text
┌──────────────────────────────────────────────────────────────┐
│ mini-agent   model   api type   status                       │
├──────────────────────────────────────────────────────────────┤
│                                                              │
│                  scrollable message viewport                │
│                                                              │
├──────────────────────────────────────────────────────────────┤
│ composer                                                     │
│ multi-line input                                             │
├──────────────────────────────────────────────────────────────┤
│ Enter send  Shift+Enter newline  PageUp/PageDown scroll      │
└──────────────────────────────────────────────────────────────┘
```

- `Enter` submits a non-empty composer.
- `Shift+Enter` inserts a newline when the terminal reports the modifier.
- `Up`, `Down`, `Home`, and `End` edit the composer while it has focus.
- `PageUp` and `PageDown` scroll the message viewport.
- `Ctrl+u` and `Ctrl+d` scroll by one viewport.
- `End` in the message viewport returns to the newest content and clears the
  unread indicator.
- `Esc` clears the current composer content.
- `Ctrl+c` exits from any state. `q` exits only when the composer is empty and
  no request is active.

The message viewport starts in follow mode. Any manual scroll disables follow
mode; new content increments an unread count until the user returns to the
bottom.

## Error Handling and Lifecycle

Terminal setup enters alternate-screen and raw mode before the first draw. A
small guard owns teardown and is dropped on all return paths. The application
installs a panic hook that restores the terminal before delegating to the
original hook. Setup, input, draw, and stream errors are returned as
`anyhow::Result` from the runner; request failures are also shown in the
message viewport so the session remains usable when possible.

The worker stream and terminal input are multiplexed by the same async event
loop with a short redraw interval. This keeps streamed text visible without
blocking keypress handling. The runner cancels the worker on exit. It exits
when `Ctrl+c` is pressed or the user selects `q` in the idle empty-composer
state.

## Testing

- `AppState` tests verify prompt submission, text chunk accumulation, tool
  status projection, stream failure state, and follow/unread behavior.
- Composer tests verify insertion, deletion, cursor movement, and newline
  handling.
- Scroll tests verify clamping, page movement, and return-to-bottom behavior.
- The CLI wiring is verified with workspace compilation and the existing
  mapping/configuration tests.
- Manual smoke testing runs `cargo run -p agent-cli` in a configured terminal
  and checks normal exit plus a streamed response.

## Compatibility and Rollout

The existing command remains `cargo run -p agent-cli`; only its presentation
layer changes from line-oriented output to the full-screen TUI. Existing TOML
configuration keys and provider API behavior remain unchanged. The change is
local and reversible by restoring the previous CLI loop if terminal library
compatibility issues are found.
