use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use crate::app::App;
use crate::article::{BlockKind, ImageCell};
use crate::models::ReadStatus;
use crate::speed::SpeedReader;

use super::theme;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(area);

    let reader_progress;
    let reader_meta;
    let reader_search;
    let speed_reading;

    {
        let Some(reader) = app.reader.as_mut() else {
            return;
        };
        speed_reading = reader.speed.is_some();

        let block = Block::bordered()
            .border_style(Style::new().fg(theme::border()))
            .padding(Padding::horizontal(1));
        let inner = block.inner(body_area);

        let content_width = if reader.max_text_width == 0 {
            inner.width
        } else {
            inner
                .width
                .min(reader.max_text_width.clamp(12, u16::MAX as usize) as u16)
        }
        .max(1);
        let offset = if reader.center_text {
            inner.width.saturating_sub(content_width) / 2
        } else {
            0
        };
        let content_area =
            Rect::new(inner.x + offset, inner.y, content_width, inner.height);

        reader.viewport = inner.height.max(1) as usize;
        reader.reflow(content_width.max(12) as usize);
        let scroll = reader.scroll;
        reader.scroll_to(scroll);
        let live_progress = reader.progress();

        let bookmark = &reader.bookmark;
        let (icon, icon_color) = match bookmark.status() {
            ReadStatus::Unread => ("●", theme::accent()),
            ReadStatus::Reading => ("◐", theme::accent_alt()),
            ReadStatus::Read => ("○", theme::faint()),
        };

        let mut title_spans = vec![
            Span::styled(format!("{icon} "), Style::new().fg(icon_color)),
            Span::styled(
                super::truncate_width(
                    &bookmark.display_title(),
                    area.width.saturating_sub(6) as usize,
                ),
                Style::new()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if bookmark.is_marked {
            title_spans
                .push(Span::styled(" ★", Style::new().fg(theme::accent_alt())));
        }
        if bookmark.is_archived {
            title_spans
                .push(Span::styled(" ■", Style::new().fg(theme::faint())));
        }

        let mut meta_parts = Vec::new();
        let host = bookmark.host();
        if !host.is_empty() {
            meta_parts.push(host);
        }
        let authors = bookmark.author_line();
        if !authors.is_empty() {
            meta_parts.push(authors);
        }
        let date = super::human_date(bookmark.published_or_created());
        if !date.is_empty() {
            meta_parts.push(date);
        }
        if bookmark.reading_time > 0 {
            meta_parts.push(format!("{} min read", bookmark.reading_time));
        }
        if bookmark.word_count > 0 {
            meta_parts.push(format!("{} words", bookmark.word_count));
        }
        if !bookmark.labels.is_empty() {
            meta_parts.push(
                bookmark
                    .labels
                    .iter()
                    .map(|label| format!("#{label}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        if live_progress > 0 {
            meta_parts.push(format!("{live_progress}% read"));
        }
        reader_meta = super::truncate_width(
            &meta_parts.join("  ·  "),
            area.width.saturating_sub(4) as usize,
        );

        let header = Paragraph::new(vec![
            Line::from(title_spans),
            Line::from(Span::styled(
                reader_meta.clone(),
                Style::new().fg(theme::dim()),
            )),
            Line::from(Span::styled(
                super::truncate_width(
                    &bookmark.url,
                    area.width.saturating_sub(4) as usize,
                ),
                Style::new().fg(theme::faint()),
            )),
        ]);
        frame.render_widget(header, header_area);
        frame.render_widget(block, body_area);

        if reader.loading {
            centered(
                frame,
                inner,
                &[Line::from(Span::styled(
                    format!("{}  Loading article…", super::spinner(app.tick)),
                    Style::new().fg(theme::dim()),
                ))],
            );
        } else if let Some(error) = &reader.error {
            centered(
                frame,
                inner,
                &[
                    Line::from(Span::styled(
                        "Could not load the article",
                        Style::new()
                            .fg(theme::red())
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        super::truncate_width(
                            error,
                            inner.width.saturating_sub(4) as usize,
                        ),
                        Style::new().fg(theme::dim()),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "Press r to retry · Esc to go back",
                        Style::new().fg(theme::faint()),
                    )),
                ],
            );
        } else if reader.lines.is_empty() {
            centered(
                frame,
                inner,
                &[Line::from(Span::styled(
                    "This bookmark has no article text.",
                    Style::new().fg(theme::dim()),
                ))],
            );
        } else if let Some(speed) = &reader.speed {
            draw_speed(frame, inner, speed, content_width as usize);
        } else {
            let mut annotated_blocks = std::collections::HashMap::new();
            for annotation in &reader.annotations {
                if let Some(block) = reader.annotation_block(annotation) {
                    annotated_blocks.entry(block).or_insert_with(|| {
                        super::annotation_color(annotation.color())
                    });
                }
            }
            let visible: Vec<Line> = reader
                .lines
                .iter()
                .skip(reader.scroll)
                .take(reader.viewport)
                .map(|line| {
                    let mut rendered = if let Some(cells) = &line.image {
                        image_line(cells, content_width as usize)
                    } else {
                        article_line(
                            &line.text,
                            block_style(line.kind),
                            reader.search_query.as_deref(),
                        )
                    };
                    if let Some(color) = line
                        .block
                        .and_then(|block| annotated_blocks.get(&block))
                        .copied()
                    {
                        for span in &mut rendered.spans {
                            span.style = span.style.fg(color);
                        }
                    }
                    rendered
                })
                .collect();
            frame.render_widget(Paragraph::new(visible), content_area);

            let max = reader.max_scroll().saturating_add(1);
            if max > 1 {
                let mut state = ScrollbarState::new(max)
                    .position(reader.scroll.min(max.saturating_sub(1)));
                frame.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight)
                        .style(Style::new().fg(theme::border())),
                    body_area,
                    &mut state,
                );
            }
        }

        reader_progress = match &reader.speed {
            Some(speed) => {
                let state = if speed.playing() {
                    ""
                } else if speed.index() + 1 >= speed.total() {
                    " · finished"
                } else {
                    " · paused"
                };
                format!(
                    "word {} of {} · {} wpm{state}",
                    speed.index() + 1,
                    speed.total(),
                    speed.wpm()
                )
            }
            None => format!(
                "{live_progress}% read · line {} of {}",
                (reader.scroll + 1).min(reader.lines.len().max(1)),
                reader.lines.len()
            ),
        };
        reader_search = reader.search_query.as_ref().map(|query| {
            format!(
                "search “{}” · {} matches ",
                super::truncate_width(query, 24),
                reader.search_match_lines().len()
            )
        });
    }

    let [footer_top, footer_bottom] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
            .areas(footer_area);

    let hints = if speed_reading {
        "  Space pause/resume · h/l word · +/- words per minute · s or Esc back to article"
    } else {
        "  j/k scroll · Space paragraph · Ctrl+d/u half · g/G top/bottom · H annotations · s speed read · m read · f favorite · a archive · t labels · i images · o browser · Esc back"
    };
    let top = if let Some(toast) = super::toast_line(app) {
        toast
    } else {
        Line::from(Span::styled(
            super::truncate_width(hints, area.width as usize),
            Style::new().fg(theme::faint()),
        ))
    };
    frame.render_widget(
        Paragraph::new(top).style(Style::new().bg(theme::bg_bar())),
        footer_top,
    );

    let right = reader_search.unwrap_or_else(|| {
        format!(
            "{} ",
            super::truncate_width(
                &reader_meta,
                (area.width as usize / 2).max(10)
            )
        )
    });
    let [left_area, right_area] = Layout::horizontal([
        Constraint::Min(18),
        Constraint::Length(area.width / 2),
    ])
    .areas(footer_bottom);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("  {reader_progress}"),
            Style::new().fg(theme::dim()),
        )))
        .style(Style::new().bg(theme::bg_bar())),
        left_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            right,
            Style::new().fg(theme::faint()),
        )))
        .alignment(Alignment::Right)
        .style(Style::new().bg(theme::bg_bar())),
        right_area,
    );
}

fn block_style(kind: Option<BlockKind>) -> Style {
    match kind {
        None => Style::new(),
        Some(BlockKind::Paragraph | BlockKind::ListItem) => {
            Style::new().fg(theme::text())
        }
        Some(BlockKind::Heading1) => Style::new()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD),
        Some(BlockKind::Heading2) => Style::new()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD),
        Some(BlockKind::Heading3) => {
            Style::new().fg(theme::text()).add_modifier(Modifier::BOLD)
        }
        Some(BlockKind::Quote) => {
            Style::new().fg(theme::dim()).add_modifier(Modifier::ITALIC)
        }
        Some(BlockKind::Code) => Style::new().fg(theme::green()),
        Some(BlockKind::Caption) => {
            Style::new().fg(theme::dim()).add_modifier(Modifier::ITALIC)
        }
        Some(BlockKind::Rule) => Style::new().fg(theme::border()),
        Some(BlockKind::Image) => Style::new()
            .fg(theme::faint())
            .add_modifier(Modifier::ITALIC),
    }
}

fn article_line(text: &str, base: Style, query: Option<&str>) -> Line<'static> {
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return Line::from(Span::styled(text.to_string(), base));
    };
    let lower = text.to_ascii_lowercase();
    let needle = query.to_ascii_lowercase();
    if !lower.contains(&needle) {
        return Line::from(Span::styled(text.to_string(), base));
    }

    let highlight = Style::new()
        .fg(theme::bg_base())
        .bg(theme::accent_alt())
        .add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    let mut start = 0;
    while let Some(relative) = lower[start..].find(&needle) {
        let match_start = start + relative;
        let match_end = match_start + needle.len();
        if match_start > start {
            spans
                .push(Span::styled(text[start..match_start].to_string(), base));
        }
        spans.push(Span::styled(
            text[match_start..match_end].to_string(),
            highlight,
        ));
        start = match_end;
        if start >= text.len() {
            break;
        }
    }
    if start < text.len() {
        spans.push(Span::styled(text[start..].to_string(), base));
    }
    Line::from(spans)
}

fn image_line(cells: &[ImageCell], width: usize) -> Line<'static> {
    let (bg_r, bg_g, bg_b) = match theme::bg_base() {
        ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    };
    let blend = |pixel: [u8; 4]| {
        let alpha = pixel[3] as u16;
        let inverse = 255 - alpha;
        let channel = |value: u8, background: u8| {
            ((value as u16 * alpha + background as u16 * inverse) / 255) as u8
        };
        ratatui::style::Color::Rgb(
            channel(pixel[0], bg_r),
            channel(pixel[1], bg_g),
            channel(pixel[2], bg_b),
        )
    };

    let padding = width.saturating_sub(cells.len()) / 2;
    let mut spans = Vec::with_capacity(cells.len() + 1);
    if padding > 0 {
        spans.push(Span::raw(" ".repeat(padding)));
    }
    for cell in cells {
        spans.push(Span::styled(
            "▀",
            Style::new().fg(blend(cell.top)).bg(blend(cell.bottom)),
        ));
    }
    Line::from(spans)
}

fn draw_speed(
    frame: &mut Frame,
    area: Rect,
    speed: &SpeedReader,
    width: usize,
) {
    let word = super::truncate_width(speed.current(), width.max(1));
    let status = if speed.playing() {
        format!("{} wpm", speed.wpm())
    } else if speed.index() + 1 >= speed.total() {
        format!("{} wpm · finished — Space to restart", speed.wpm())
    } else {
        format!("{} wpm · paused — Space to resume", speed.wpm())
    };
    let lines = vec![
        Line::from(Span::styled(
            word,
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(status, Style::new().fg(theme::faint()))),
    ];
    centered(frame, area, &lines);
}

fn centered(frame: &mut Frame, area: Rect, lines: &[Line<'static>]) {
    let height = lines.len() as u16;
    let y = area.y + area.height.saturating_sub(height) / 2;
    frame.render_widget(
        Paragraph::new(lines.to_vec()).alignment(Alignment::Center),
        Rect::new(area.x, y, area.width, height.min(area.height)),
    );
}
