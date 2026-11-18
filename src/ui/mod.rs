mod list;
mod modal;
mod reader;
mod setup;
mod sidebar;
pub mod theme;

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Screen, ToastKind};
use crate::models::Bookmark;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::new().bg(theme::bg_base())),
        area,
    );
    if area.width < 36 || area.height < 8 {
        draw_too_small(frame, area);
        return;
    }

    match app.screen {
        Screen::Setup => setup::draw(frame, app),
        Screen::Main => {
            if app.reader.is_some() {
                reader::draw(frame, app);
            } else {
                draw_main(frame, app);
            }
        }
    }

    if app.modal.is_some() {
        modal::draw(frame, app);
    }
}

fn draw_main(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let [body_area, footer_area] =
        Layout::vertical([Constraint::Min(4), Constraint::Length(2)])
            .areas(area);

    draw_footer(frame, app, footer_area);

    let sidebar_width = sidebar_width(app, body_area.width);
    let [sidebar_area, list_area] = Layout::horizontal([
        Constraint::Length(sidebar_width),
        Constraint::Min(16),
    ])
    .areas(body_area);

    sidebar::draw(frame, app, sidebar_area);
    list::draw(frame, app, list_area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let [status_area, hints_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
            .areas(area);

    let right_width = (area.width / 2).clamp(16, 44);
    let [status_left, status_right] = Layout::horizontal([
        Constraint::Min(10),
        Constraint::Length(right_width),
    ])
    .areas(status_area);

    frame.render_widget(
        Paragraph::new(status_line(app))
            .style(Style::new().bg(theme::bg_bar())),
        status_left,
    );

    let counts = if app.loading_list {
        format!("{} loading… ", spinner(app.tick))
    } else {
        format!(
            "{}/{} · {} ",
            app.bookmarks.len(),
            app.total,
            app.sort.label()
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            counts,
            Style::new().fg(theme::dim()),
        )))
        .alignment(Alignment::Right)
        .style(Style::new().bg(theme::bg_bar())),
        status_right,
    );

    let hints = match app.focus {
        crate::app::Focus::Sidebar => {
            "  j/k move · / filter · Enter apply · R rename label · PgUp/PgDn page · Ctrl+d/u · Tab/Esc back · ? help"
        }
        crate::app::Focus::List => {
            if app.filter == crate::models::Filter::Highlights {
                "  j/k move · Enter open highlight · PgUp/PgDn page · r refresh · Tab filters · ? help"
            } else {
                "  j/k move · Space select · Enter read · m read · f favorite · a archive · t labels · / search · n add · Tab filters · ? help"
            }
        }
    };
    let offline_width = if app.offline { 9 } else { 0 };
    let [hints_left, hints_right] = Layout::horizontal([
        Constraint::Min(16),
        Constraint::Length(offline_width),
    ])
    .areas(hints_area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_width(hints, hints_left.width as usize),
            Style::new().fg(theme::faint()),
        )))
        .style(Style::new().bg(theme::bg_bar())),
        hints_left,
    );
    if app.offline {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " OFFLINE ",
                Style::new()
                    .fg(theme::accent_alt())
                    .add_modifier(Modifier::BOLD),
            )))
            .alignment(Alignment::Right)
            .style(Style::new().bg(theme::bg_bar())),
            hints_right,
        );
    }
}

pub(crate) fn toast_line(app: &App) -> Option<Line<'static>> {
    let toast = app.toast.as_ref()?;
    let style = match toast.kind {
        ToastKind::Info => Style::new().fg(theme::accent()),
        ToastKind::Success => Style::new().fg(theme::green()),
        ToastKind::Error => {
            Style::new().fg(theme::red()).add_modifier(Modifier::BOLD)
        }
    };
    Some(Line::from(Span::styled(format!(" {}", toast.text), style)))
}

pub(crate) fn status_line(app: &App) -> Line<'static> {
    if let Some(line) = toast_line(app) {
        return line;
    }
    let mut segments: Vec<(String, Style)> = Vec::new();
    let pending = app.pending_changes();
    if pending > 0 {
        segments.push((
            format!("{pending} pending"),
            Style::new()
                .fg(theme::purple())
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !app.search.is_empty() {
        segments.push((
            format!("“{}”", truncate_width(&app.search, 28)),
            Style::new().fg(theme::accent_alt()),
        ));
    }
    if let Some(error) = &app.list_error {
        segments
            .push((truncate_width(error, 140), Style::new().fg(theme::red())));
    } else if let Some(bookmark) = app.selected_bookmark() {
        if !bookmark.url.is_empty() {
            segments.push((
                truncate_width(&bookmark.url, 140),
                Style::new().fg(theme::faint()),
            ));
        }
    }

    if segments.is_empty() {
        return Line::from(Span::styled(
            " Ready",
            Style::new().fg(theme::faint()),
        ));
    }
    let mut spans = vec![Span::raw(" ")];
    for (index, (text, style)) in segments.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", Style::new().fg(theme::faint())));
        }
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

/// Map a Readeck annotation color to the theme palette.
pub(crate) fn annotation_color(name: &str) -> ratatui::style::Color {
    match name.to_ascii_lowercase().as_str() {
        "red" => theme::red(),
        "green" => theme::green(),
        "blue" => theme::accent(),
        "purple" => theme::purple(),
        _ => theme::accent_alt(),
    }
}

pub(crate) fn spinner(tick: u64) -> &'static str {
    const FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    FRAMES[(tick as usize) % FRAMES.len()]
}

pub(crate) fn truncate_width(value: &str, max: usize) -> String {
    if value.width() <= max {
        return value.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in value.chars() {
        let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + width >= max {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}

pub(crate) fn human_date(date: Option<DateTime<Utc>>) -> String {
    let Some(date) = date else {
        return String::new();
    };
    let days = Utc::now()
        .date_naive()
        .signed_duration_since(date.date_naive())
        .num_days();
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=6 => format!("{days} days ago"),
        _ => date.format("%Y-%m-%d").to_string(),
    }
}

pub(crate) fn bookmark_meta(bookmark: &Bookmark) -> String {
    let mut parts = Vec::new();
    let host = bookmark.host();
    if !host.is_empty() {
        parts.push(host);
    }
    if bookmark.reading_time > 0 {
        parts.push(format!("{} min", bookmark.reading_time));
    }
    if (1..100).contains(&bookmark.read_progress) {
        parts.push(format!("{}%", bookmark.read_progress));
    }
    if !bookmark.labels.is_empty() {
        let mut labels: Vec<String> = bookmark
            .labels
            .iter()
            .take(3)
            .map(|label| format!("#{label}"))
            .collect();
        if bookmark.labels.len() > 3 {
            labels.push(format!("+{}", bookmark.labels.len() - 3));
        }
        parts.push(labels.join(" "));
    }
    let date = human_date(bookmark.published_or_created());
    if !date.is_empty() {
        parts.push(date);
    }
    parts.join(" · ")
}

pub(crate) fn sidebar_width(app: &App, available: u16) -> u16 {
    let longest = app
        .labels
        .iter()
        .map(|label| label.name.width() + 3)
        .chain(
            app.collections
                .iter()
                .map(|collection| collection.name.width() + 4),
        )
        .max()
        .unwrap_or(0);
    let desired = (longest as u16 + 8).clamp(22, 34);
    desired.min(available.saturating_sub(16).max(18))
}

fn draw_too_small(frame: &mut Frame, area: Rect) {
    let message = vec![
        Line::from(Span::styled(
            "Terminal too small",
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("Need at least 36×8, have {}×{}", area.width, area.height),
            Style::new().fg(theme::dim()),
        )),
    ];
    let height = message.len() as u16;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let paragraph = Paragraph::new(message).alignment(Alignment::Center);
    frame.render_widget(
        paragraph,
        Rect::new(area.x, y, area.width, height.min(area.height)),
    );
}
