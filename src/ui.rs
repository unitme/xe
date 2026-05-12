use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::app::App;

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(6),
            Constraint::Length(3),
        ])
        .split(area);

    draw_status(frame, chunks[0], app);
    draw_tracks(frame, chunks[1], app);
    draw_footer(frame, chunks[2], app);
}

fn draw_status(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &App) {
    let playing = app
        .playing_index
        .and_then(|index| app.playlist.entries.get(index))
        .map(|entry| entry.query.as_str())
        .unwrap_or("idle");
    let title = format!(
        "{} - {} entries - mode {} - now {}",
        app.playlist.name,
        app.playlist.entries.len(),
        app.mode,
        playing
    );

    let title_line = Line::from(vec![
        Span::styled(
            "Play",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(title),
    ]);

    let mut lines = Vec::with_capacity(4);
    lines.push(title_line);
    let recent = app.status_log.iter().rev().take(3).collect::<Vec<_>>();
    for status in recent.into_iter().rev() {
        lines.push(Line::raw(format!("Status: {status}")));
    }

    let status = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(status, area);
}

fn draw_tracks(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &App) {
    let visible_rows = area.height.saturating_sub(2).max(1) as usize;
    let start = if app.selected >= visible_rows {
        app.selected + 1 - visible_rows
    } else {
        0
    };
    let end = (start + visible_rows).min(app.playlist.entries.len());

    let items = app.playlist.entries[start..end]
        .iter()
        .enumerate()
        .map(|(offset, entry)| {
            let index = start + offset;
            let selected = index == app.selected;
            let playing = Some(index) == app.playing_index;
            let style = if selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else if playing {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };
            let marker = if selected { ">" } else { " " };
            let play = if playing {
                if app.paused { "PAUSE" } else { "PLAY " }
            } else {
                "     "
            };
            let pinned = if entry.resolved_id.is_some() {
                "yt"
            } else {
                "--"
            };
            ListItem::new(Line::from(vec![Span::raw(format!(
                "{marker} {play} {:03}. {} [{}]",
                index + 1,
                entry.query,
                pinned
            ))]))
            .style(style)
        })
        .collect::<Vec<_>>();

    let list = List::new(items).block(Block::default().title("Playlist").borders(Borders::ALL));
    frame.render_widget(list, area);
}

fn draw_footer(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &App) {
    let muted = if app.muted { " muted" } else { "" };
    let footer = Paragraph::new(format!(
        "[Enter] play  [Space/P] pause  [N/B] next/prev  [M] mode  [S] stop  [+/−] volume  [Q] quit | Volume: {}{}",
        app.volume, muted
    ))
    .block(Block::default().borders(Borders::ALL))
    .wrap(Wrap { trim: true });
    frame.render_widget(footer, area);
}
