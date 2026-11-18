use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph};

use crate::app::{App, Focus};
use crate::models::ReadStatus;

use super::theme;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::List;
    let border = if focused {
        theme::accent()
    } else {
        theme::border()
    };
    let mut title_spans = vec![
        Span::styled(
            " Bookmarks ",
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} ", app.total),
            Style::new().fg(theme::faint()),
        ),
    ];
    if !app.selected_ids.is_empty() {
        title_spans.push(Span::styled(
            format!("· {} selected ", app.selected_ids.len()),
            Style::new().fg(theme::green()).add_modifier(Modifier::BOLD),
        ));
    }
    let title = Line::from(title_spans);
    let block = Block::bordered()
        .border_style(Style::new().fg(border))
        .title(title)
        .title_bottom(Line::from(Span::styled(
            format!(" {} ", app.sort.label()),
            Style::new().fg(theme::faint()),
        )));
    let inner = block.inner(area);
    app.list_viewport = (inner.height / 2).max(1) as usize;
    frame.render_widget(block, area);

    if app.bookmarks.is_empty() {
        draw_empty(frame, app, inner);
        return;
    }

    let items: Vec<ListItem> = app
        .bookmarks
        .iter()
        .map(|bookmark| {
            let (icon, icon_color) = match bookmark.status() {
                ReadStatus::Unread => ("●", theme::accent()),
                ReadStatus::Reading => ("◐", theme::accent_alt()),
                ReadStatus::Read => ("○", theme::faint()),
            };

            let mut first = Vec::new();
            if app.selected_ids.contains(&bookmark.id) {
                first.push(Span::styled(
                    "✓ ",
                    Style::new()
                        .fg(theme::green())
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                first.push(Span::raw("  "));
            }
            first.push(Span::styled(
                format!("{icon} "),
                Style::new().fg(icon_color),
            ));
            if bookmark.has_error() {
                first.push(Span::styled("! ", Style::new().fg(theme::red())));
            } else if bookmark.is_loading() {
                first.push(Span::styled(
                    "… ",
                    Style::new().fg(theme::accent_alt()),
                ));
            }

            let mut title_style = Style::new().fg(theme::text());
            if bookmark.status() == ReadStatus::Unread {
                title_style = title_style.add_modifier(Modifier::BOLD);
            } else if bookmark.is_read() {
                title_style = title_style.fg(theme::dim());
            }
            first.push(Span::styled(bookmark.display_title(), title_style));

            if bookmark.is_marked {
                first.push(Span::styled(
                    " ★",
                    Style::new().fg(theme::accent_alt()),
                ));
            }
            if bookmark.is_archived {
                first.push(Span::styled(" ■", Style::new().fg(theme::faint())));
            }
            if bookmark
                .note
                .as_deref()
                .is_some_and(|note| !note.trim().is_empty())
            {
                first
                    .push(Span::styled(" ✎", Style::new().fg(theme::purple())));
            }

            let annotations = app.annotations_for(&bookmark.id);
            let mut meta = super::bookmark_meta(bookmark);
            if !annotations.is_empty() {
                meta.push_str(&format!(
                    " · {} annotation{}",
                    annotations.len(),
                    if annotations.len() == 1 { "" } else { "s" }
                ));
            }
            let mut lines = vec![
                Line::from(first),
                Line::from(Span::styled(
                    format!("    {meta}"),
                    Style::new().fg(theme::dim()),
                )),
            ];
            let width = inner.width.saturating_sub(8) as usize;
            for annotation in annotations {
                let text = annotation.text.trim();
                let text = if text.is_empty() {
                    annotation.note.trim()
                } else {
                    text
                };
                if text.is_empty() {
                    continue;
                }
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        "▍ ",
                        Style::new()
                            .fg(super::annotation_color(annotation.color())),
                    ),
                    Span::styled(
                        super::truncate_width(&text.replace('\n', " "), width),
                        Style::new().fg(theme::dim()),
                    ),
                ]));
            }

            ListItem::new(lines)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::new()
                .bg(theme::bg_selected())
                .fg(theme::text())
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌ ");
    frame.render_stateful_widget(list, inner, &mut app.list_state);
}

fn draw_empty(frame: &mut Frame, app: &App, area: Rect) {
    let lines = if app.loading_list {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("{}  Loading bookmarks…", super::spinner(app.tick)),
                Style::new().fg(theme::dim()),
            )),
        ]
    } else if let Some(error) = &app.list_error {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                super::truncate_width(
                    error,
                    area.width.saturating_sub(4) as usize,
                ),
                Style::new().fg(theme::red()),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Press r to retry.",
                Style::new().fg(theme::dim()),
            )),
        ]
    } else if !app.search.is_empty() {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "No bookmarks match your search.",
                Style::new().fg(theme::dim()),
            )),
            Line::from(Span::styled(
                "Press / and submit an empty query to clear it.",
                Style::new().fg(theme::faint()),
            )),
        ]
    } else {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "No bookmarks here.",
                Style::new().fg(theme::dim()),
            )),
            Line::from(Span::styled(
                "Press n to save a link, or choose another view.",
                Style::new().fg(theme::faint()),
            )),
        ]
    };

    let height = lines.len() as u16;
    let y = area.y + area.height.saturating_sub(height) / 2;
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        Rect::new(area.x, y, area.width, height.min(area.height)),
    );
}
