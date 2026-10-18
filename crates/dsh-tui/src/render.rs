//! Screen layout for the TUI.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::app::{App, Item, Status};

/// Draw the whole screen.
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);
    render_transcript(frame, chunks[0], app);
    render_input(frame, chunks[1], app);
    render_status(frame, chunks[2], app);
    if app.approval().is_some() {
        render_approval(frame, area, app);
    }
}

fn transcript_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for item in app.items() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        match item {
            Item::User(text) => {
                push_prefixed(
                    &mut lines,
                    text,
                    "> ",
                    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                );
            }
            Item::Assistant(text) => {
                for line in text.split('\n') {
                    lines.push(Line::raw(line.to_string()));
                }
            }
            Item::ToolCall { name, preview } => {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("tool {name} "),
                        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(preview.clone(), Style::new().fg(Color::DarkGray)),
                ]));
            }
            Item::ToolResult { content, is_error } => {
                let style = if *is_error {
                    Style::new().fg(Color::Red)
                } else {
                    Style::new().fg(Color::DarkGray)
                };
                lines.push(Line::from(Span::styled(
                    if *is_error { "error" } else { "result" },
                    style.add_modifier(Modifier::BOLD),
                )));
                for line in content.split('\n') {
                    lines.push(Line::from(Span::styled(line.to_string(), style)));
                }
            }
            Item::Notice(text) => {
                lines.push(Line::from(Span::styled(
                    text.clone(),
                    Style::new()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
            Item::Error(text) => {
                lines.push(Line::from(Span::styled(
                    format!("error: {text}"),
                    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
                )));
            }
        }
    }
    lines
}

fn push_prefixed(lines: &mut Vec<Line<'static>>, text: &str, prefix: &str, style: Style) {
    for (index, line) in text.split('\n').enumerate() {
        if index == 0 {
            lines.push(Line::from(vec![
                Span::styled(prefix.to_string(), style),
                Span::styled(line.to_string(), style),
            ]));
        } else {
            lines.push(Line::from(Span::styled(line.to_string(), style)));
        }
    }
}

fn visual_lines(lines: &[Line<'_>], width: u16) -> usize {
    let width = usize::from(width.max(1));
    lines
        .iter()
        .map(|line| {
            let characters = line.width();
            characters.max(1).div_ceil(width)
        })
        .sum()
}

fn render_transcript(frame: &mut Frame, area: Rect, app: &mut App) {
    let lines = transcript_lines(app);
    let inner_width = area.width.saturating_sub(2);
    let total = visual_lines(&lines, inner_width);
    let viewport = area.height.saturating_sub(2);
    app.set_viewport(u16::try_from(total).unwrap_or(u16::MAX), viewport);
    let block = Block::bordered()
        .title(" dsh ")
        .border_style(Style::new().fg(Color::DarkGray));
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.scroll(), 0));
    frame.render_widget(paragraph, area);
}

fn render_input(frame: &mut Frame, area: Rect, app: &App) {
    let title = match app.status() {
        Status::Ready => " message ",
        _ => " message (wait for the turn or press Esc) ",
    };
    let block = Block::bordered()
        .title(title)
        .border_style(Style::new().fg(Color::DarkGray));
    let input = Paragraph::new(Line::from(vec![
        Span::styled("> ", Style::new().fg(Color::Cyan)),
        Span::raw(app.input().to_string()),
    ]))
    .block(block);
    frame.render_widget(input, area);
    if let Some(position) = input_cursor(area, app) {
        frame.set_cursor_position(position);
    }
}

fn input_cursor(area: Rect, app: &App) -> Option<(u16, u16)> {
    if app.status() != Status::Ready {
        return None;
    }
    let x = area
        .x
        .saturating_add(3)
        .saturating_add(u16::try_from(app.input().chars().count()).unwrap_or(u16::MAX))
        .min(area.right().saturating_sub(2));
    Some((x, area.y.saturating_add(1)))
}

fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let hints = "enter send | esc cancel | pgup/pgdn scroll | ctrl-c quit";
    let status = format!(
        " {} | {} | session {} ",
        app.model(),
        app.status().label(),
        app.session()
    );
    let line = Line::from(vec![
        Span::styled(status, Style::new().fg(Color::DarkGray)),
        Span::styled(
            hints,
            Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn approval_rect(area: Rect) -> Rect {
    let width = area.width.saturating_mul(2) / 3;
    let height = area.height.min(9);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn render_approval(frame: &mut Frame, area: Rect, app: &App) {
    let Some(call) = app.approval() else {
        return;
    };
    let popup = approval_rect(area);
    frame.render_widget(Clear, popup);
    let preview: String = call.arguments.chars().take(400).collect();
    let lines = vec![
        Line::from(Span::styled(
            call.name.clone(),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(preview),
        Line::default(),
        Line::from(Span::styled(
            "y approve    n deny",
            Style::new().fg(Color::Cyan),
        )),
    ];
    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::bordered()
                .title(" approve tool call ")
                .border_style(Style::new().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::app::{App, WorkerEvent};
    use dsh_agent::ToolCall;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn draw(app: &mut App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| render(frame, app)).expect("draw");
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(usize::from(buffer.area.width))
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_screen_shows_the_transcript_input_and_status() {
        let mut app = App::new("deepseek-chat", "session-1");
        app.apply(WorkerEvent::Delta("Hello there".into()));
        let screen = draw(&mut app);
        assert!(screen.contains("dsh"));
        assert!(screen.contains("Hello there"));
        assert!(screen.contains("deepseek-chat"));
        assert!(screen.contains("streaming"));
        assert!(screen.contains("session-1"));
        assert!(screen.contains("message"));
    }

    #[test]
    fn the_user_message_is_marked_and_the_input_is_visible() {
        let mut app = App::new("m", "s");
        app.apply(WorkerEvent::Delta("reply".into()));
        app.apply(WorkerEvent::TurnDone);
        app.apply(WorkerEvent::Notice("ready".into()));
        let screen = draw(&mut app);
        assert!(screen.contains("reply"));
        assert!(screen.contains("ready"));
    }

    #[test]
    fn the_approval_overlay_shows_the_tool_and_the_choices() {
        let mut app = App::new("m", "s");
        app.apply(WorkerEvent::Approval(ToolCall {
            id: "c".into(),
            name: "bash".into(),
            arguments: "{\"command\":\"rm -rf build\"}".into(),
        }));
        let screen = draw(&mut app);
        assert!(screen.contains("approve tool call"));
        assert!(screen.contains("bash"));
        assert!(screen.contains("rm -rf build"));
        assert!(screen.contains("y approve"));
    }

    #[test]
    fn tool_results_and_errors_render() {
        let mut app = App::new("m", "s");
        app.apply(WorkerEvent::ToolResult {
            content: "exit 1".into(),
            is_error: true,
        });
        app.apply(WorkerEvent::Error("provider failed".into()));
        let screen = draw(&mut app);
        assert!(screen.contains("error"));
        assert!(screen.contains("exit 1"));
        assert!(screen.contains("provider failed"));
    }

    #[test]
    fn the_ready_input_does_not_show_the_busy_title() {
        let mut app = App::new("m", "s");
        let ready = draw(&mut app);
        assert!(!ready.contains("wait for the turn"), "{ready}");
        app.apply(WorkerEvent::Delta("x".into()));
        let busy = draw(&mut app);
        assert!(busy.contains("wait for the turn"), "{busy}");
    }

    #[test]
    fn an_empty_app_still_renders() {
        let mut app = App::new("m", "s");
        let screen = draw(&mut app);
        assert!(screen.contains("ready"));
        assert!(screen.contains('m'));
    }
}

#[cfg(test)]
mod structure_tests {
    use super::{approval_rect, input_cursor, transcript_lines};
    use crate::app::{App, WorkerEvent};
    use ratatui::layout::Rect;

    #[test]
    fn only_the_first_line_of_a_user_message_carries_the_prefix() {
        let mut app = App::new("m", "s");
        app.submit("first\nsecond");
        let lines = transcript_lines(&app);
        assert_eq!(lines[0].to_string(), "> first");
        assert_eq!(lines[1].to_string(), "second");
    }

    #[test]
    fn items_are_separated_by_one_blank_line_and_never_lead_with_one() {
        let mut app = App::new("m", "s");
        app.submit("hello");
        app.apply(WorkerEvent::Delta("reply".into()));
        let lines = transcript_lines(&app);
        assert_eq!(lines[0].to_string(), "> hello");
        assert_eq!(lines[1].to_string(), "");
        assert_eq!(lines[2].to_string(), "reply");
        assert!(!lines[0].to_string().is_empty());
    }

    #[test]
    fn an_empty_app_has_no_transcript_lines() {
        assert!(transcript_lines(&App::new("m", "s")).is_empty());
    }

    #[test]
    fn visual_line_counts_cover_wrapping_and_empty_lines() {
        use super::visual_lines;
        use ratatui::text::Line;
        let lines = vec![Line::raw("abcdef"), Line::raw(""), Line::raw("a")];
        assert_eq!(visual_lines(&lines, 3), 4);
        assert_eq!(visual_lines(&lines, 10), 3);
    }

    #[test]
    fn the_approval_popup_is_centered_and_bounded() {
        assert_eq!(
            approval_rect(Rect::new(0, 0, 80, 24)),
            Rect::new(13, 7, 53, 9)
        );
        assert_eq!(
            approval_rect(Rect::new(0, 0, 30, 6)),
            Rect::new(5, 0, 20, 6)
        );
    }

    #[test]
    fn the_cursor_is_placed_only_while_ready() {
        let mut app = App::new("m", "s");
        app.handle_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('a'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(input_cursor(Rect::new(0, 10, 40, 3), &app), Some((4, 11)));
        app.apply(WorkerEvent::Delta("x".into()));
        assert_eq!(input_cursor(Rect::new(0, 10, 40, 3), &app), None);
    }
}
