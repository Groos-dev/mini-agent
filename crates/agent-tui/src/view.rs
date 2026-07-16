use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{AppState, MessageRole, RunState};

pub fn draw(frame: &mut Frame<'_>, state: &mut AppState) {
    let composer_height = (state.composer_line_count() as u16 + 2).clamp(3, 8);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_header(frame, layout[0], state);
    draw_messages(frame, layout[1], state);
    draw_composer(frame, layout[2], state);
    draw_footer(frame, layout[3], state);
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let status = match state.run_state() {
        RunState::Idle => "idle",
        RunState::Streaming => "working",
    };
    let title = format!(
        " mini-agent  ·  {}  ·  {}  ·  {} ",
        state.model(),
        state.api_type(),
        status
    );
    frame.render_widget(
        Paragraph::new(title).style(Style::default().fg(Color::Cyan)),
        area,
    );
}

fn draw_messages(frame: &mut Frame<'_>, area: Rect, state: &mut AppState) {
    let lines = message_lines(state);
    let total_lines = wrapped_line_count(&lines, area.width.saturating_sub(2));
    let paragraph = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::LEFT | Borders::RIGHT))
        .wrap(Wrap { trim: false });
    state.set_message_metrics(total_lines, area.height as usize);
    frame.render_widget(
        paragraph.scroll((state.message_scroll_top().min(u16::MAX as usize) as u16, 0)),
        area,
    );
}

fn wrapped_line_count(lines: &[Line<'_>], width: u16) -> usize {
    let width = usize::from(width.max(1));
    lines
        .iter()
        .map(|line| line.width().div_ceil(width).max(1))
        .sum()
}

fn message_lines(state: &AppState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for message in state.messages() {
        let (label, color) = match message.role {
            MessageRole::User => ("You", Color::Green),
            MessageRole::Assistant => ("Assistant", Color::Cyan),
            MessageRole::Tool => ("Tool", Color::Yellow),
            MessageRole::Error => ("Error", Color::Red),
        };
        lines.push(Line::from(Span::styled(
            format!("{label}>"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )));
        for content_line in message.content.lines() {
            lines.push(Line::from(format!("  {content_line}")));
        }
        if message.content.is_empty() {
            lines.push(Line::from("  "));
        }
        lines.push(Line::from(""));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Start a conversation",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

fn draw_composer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let title = if state.run_state() == RunState::Streaming {
        " composer · waiting for response "
    } else {
        " composer "
    };
    let paragraph = Paragraph::new(state.composer())
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);

    let cursor_before = &state.composer()[..state.composer_cursor()];
    let line = cursor_before.matches('\n').count() as u16;
    let column = cursor_before
        .rsplit('\n')
        .next()
        .map(unicode_width::UnicodeWidthStr::width)
        .unwrap_or_default() as u16;
    let cursor_y = area.y + 1 + line.min(area.height.saturating_sub(3));
    let cursor_x = area.x + 1 + column.min(area.width.saturating_sub(3));
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let mut footer =
        " Enter send  ·  Shift+Enter newline  ·  PgUp/PgDn scroll  ·  Ctrl+C exit ".to_string();
    if state.unread_count() > 0 {
        footer.push_str(&format!(" ·  {} new", state.unread_count()));
    }
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}
