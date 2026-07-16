use std::{
    io::{self, Stdout},
    panic,
    sync::Arc,
    time::Duration,
};

use agent_core::agent::{Agent, AgentEvent, AgentRunOptions};
use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;
use tokio::time::{self, MissedTickBehavior};

use crate::{AppState, view::draw};

const REDRAW_INTERVAL: Duration = Duration::from_millis(50);

pub struct TuiMetadata {
    pub model: String,
    pub api_type: String,
}

impl TuiMetadata {
    pub fn new(model: impl Into<String>, api_type: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_type: api_type.into(),
        }
    }
}

pub async fn run(agent: Agent, options: AgentRunOptions, metadata: TuiMetadata) -> Result<()> {
    let original_hook = Arc::new(panic::take_hook());
    let panic_hook = Arc::clone(&original_hook);
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        panic_hook(info);
    }));

    let result = async {
        let mut terminal = setup_terminal()?;
        let mut state = AppState::new(metadata.model, metadata.api_type);
        let (request_tx, event_rx, worker) = spawn_agent_worker(agent, options);
        let result = run_loop(&mut terminal, request_tx, event_rx, &mut state).await;
        worker.abort();
        let _ = worker.await;
        result
    }
    .await;

    restore_terminal();
    let _ = panic::take_hook();
    if let Ok(original_hook) = Arc::try_unwrap(original_hook) {
        panic::set_hook(original_hook);
    }
    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    request_tx: mpsc::UnboundedSender<String>,
    mut event_rx: mpsc::UnboundedReceiver<WorkerEvent>,
    state: &mut AppState,
) -> Result<()> {
    let mut tick = time::interval(REDRAW_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        terminal.draw(|frame| draw(frame, state))?;

        tokio::select! {
            _ = tick.tick() => {
                let streaming = state.run_state() == crate::RunState::Streaming;
                match process_terminal_events(state, streaming)? {
                    Some(UiAction::Exit) => return Ok(()),
                    Some(UiAction::Submit(prompt)) => {
                        state.begin_streaming();
                        if request_tx.send(prompt).is_err() {
                            state.record_stream_error("agent worker stopped");
                        }
                    }
                    None => {}
                }
            }
            event = event_rx.recv() => {
                match event {
                    Some(WorkerEvent::Agent(event)) => state.apply_event(event),
                    Some(WorkerEvent::Error(error)) => state.record_stream_error(error),
                    None => return Err(anyhow::anyhow!("agent worker stopped")),
                }
            }
        }
    }
}

enum WorkerEvent {
    Agent(AgentEvent),
    Error(String),
}

fn spawn_agent_worker(
    mut agent: Agent,
    options: AgentRunOptions,
) -> (
    mpsc::UnboundedSender<String>,
    mpsc::UnboundedReceiver<WorkerEvent>,
    tokio::task::JoinHandle<()>,
) {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let worker = tokio::spawn(async move {
        while let Some(prompt) = request_rx.recv().await {
            let stream = match agent.run_stream(prompt, options.clone()).await {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = event_tx.send(WorkerEvent::Error(error.to_string()));
                    continue;
                }
            };

            futures::pin_mut!(stream);
            while let Some(event) = stream.next().await {
                match event {
                    Ok(event) => {
                        let finished = matches!(event, AgentEvent::TurnFinished);
                        if event_tx.send(WorkerEvent::Agent(event)).is_err() {
                            return;
                        }
                        if finished {
                            break;
                        }
                    }
                    Err(error) => {
                        if event_tx
                            .send(WorkerEvent::Error(error.to_string()))
                            .is_err()
                        {
                            return;
                        }
                        break;
                    }
                }
            }
        }
    });
    (request_tx, event_rx, worker)
}

#[derive(Debug, Eq, PartialEq)]
enum UiAction {
    Exit,
    Submit(String),
}

fn process_terminal_events(state: &mut AppState, streaming: bool) -> Result<Option<UiAction>> {
    while event::poll(Duration::ZERO)? {
        let Event::Key(key) = event::read()? else {
            continue;
        };

        if let Some(action) = handle_key(state, key, streaming) {
            return Ok(Some(action));
        }
    }
    Ok(None)
}

fn handle_key(state: &mut AppState, key: KeyEvent, streaming: bool) -> Option<UiAction> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(UiAction::Exit);
    }

    if key.code == KeyCode::Char('q')
        && !streaming
        && state.composer().is_empty()
        && key.modifiers.is_empty()
    {
        return Some(UiAction::Exit);
    }

    match key.code {
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
            state.insert_newline();
        }
        KeyCode::Enter if !streaming => return state.submit_composer().map(UiAction::Submit),
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.insert_text(&character.to_string());
        }
        KeyCode::Backspace => state.backspace(),
        KeyCode::Delete => state.delete(),
        KeyCode::Left => state.move_cursor_left(),
        KeyCode::Right => state.move_cursor_right(),
        KeyCode::Home => state.move_cursor_home(),
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.scroll_to_bottom();
        }
        KeyCode::End => state.move_cursor_end(),
        KeyCode::PageUp => state.scroll_up(10),
        KeyCode::PageDown => state.scroll_down(10),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.scroll_up(10);
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.scroll_down(10);
        }
        KeyCode::Esc => state.clear_composer(),
        _ => {}
    }

    None
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal() {
    let _ = terminal::disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen, cursor::Show);
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEventKind, KeyEventState};

    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn enter_submits_only_when_idle() {
        let mut state = AppState::new("model", "completions");
        state.insert_text("hello");

        assert_eq!(
            handle_key(&mut state, key(KeyCode::Enter, KeyModifiers::NONE), false),
            Some(UiAction::Submit("hello".to_string()))
        );
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Enter, KeyModifiers::NONE), true),
            None
        );
    }

    #[test]
    fn control_c_and_empty_q_exit() {
        let mut state = AppState::new("model", "completions");

        assert_eq!(
            handle_key(
                &mut state,
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                true
            ),
            Some(UiAction::Exit)
        );
        assert_eq!(
            handle_key(
                &mut state,
                key(KeyCode::Char('q'), KeyModifiers::NONE),
                false
            ),
            Some(UiAction::Exit)
        );
    }
}
