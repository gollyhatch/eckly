use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph};

use crate::app::{App, Focus};
use crate::models::ReadStatus;

use super::theme;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.filter == crate::models::Filter::Highlights {
        return draw_highlights(frame, app, area);
    }
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

fn draw_highlights(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::List;
    let border = if focused {
        theme::accent()
    } else {
        theme::border()
    };
    let title = Line::from(vec![
        Span::styled(
            " Highlights ",
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} ", app.highlights.len()),
            Style::new().fg(theme::faint()),
        ),
    ]);
    let block = Block::bordered()
        .border_style(Style::new().fg(border))
        .title(title);
    let inner = block.inner(area);
    app.list_viewport = (inner.height / 2).max(1) as usize;
    frame.render_widget(block, area);

    if app.highlights.is_empty() {
        let y = inner.y + inner.height / 2;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No highlights yet.",
                Style::new().fg(theme::dim()),
            )))
            .alignment(Alignment::Center),
            Rect::new(inner.x, y, inner.width, 1),
        );
        return;
    }

    let width = inner.width.saturating_sub(4).max(8) as usize;
    let items: Vec<ListItem> = app
        .highlights
        .iter()
        .map(|annotation| {
            let color = super::annotation_color(annotation.color());
            let mut lines = Vec::new();

            let title = if annotation.bookmark_title.trim().is_empty() {
                annotation.bookmark_url.trim()
            } else {
                annotation.bookmark_title.trim()
            };
            if !title.is_empty() {
                lines.push(Line::from(Span::styled(
                    super::truncate_width(title, width),
                    Style::new().fg(theme::dim()).add_modifier(Modifier::BOLD),
                )));
            }

            let text = annotation.text.replace('\n', " ");
            let options = textwrap::Options::new(width)
                .initial_indent("▍ ")
                .subsequent_indent("  ")
                .break_words(true);
            for (index, wrapped) in
                textwrap::wrap(&text, options).into_iter().enumerate()
            {
                let wrapped = wrapped.into_owned();
                if index == 0 {
                    let content = wrapped
                        .strip_prefix("▍ ")
                        .unwrap_or(&wrapped)
                        .to_string();
                    lines.push(Line::from(vec![
                        Span::styled("▍ ", Style::new().fg(color)),
                        Span::styled(content, Style::new().fg(theme::text())),
                    ]));
                } else {
                    lines.push(Line::from(Span::styled(
                        wrapped,
                        Style::new().fg(theme::text()),
                    )));
                }
            }

            let note = annotation.note.trim().replace('\n', " ");
            if !note.is_empty() {
                let options = textwrap::Options::new(width)
                    .initial_indent("  — ")
                    .subsequent_indent("    ")
                    .break_words(true);
                for wrapped in textwrap::wrap(&note, options) {
                    lines.push(Line::from(Span::styled(
                        wrapped.into_owned(),
                        Style::new().fg(theme::dim()),
                    )));
                }
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
