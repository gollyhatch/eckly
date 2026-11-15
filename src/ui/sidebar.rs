use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Focus};
use crate::models::Filter;

use super::theme;

/// Prompt shown before the library filter text.
const SEARCH_PREFIX: &str = " / ";
const SEARCH_PREFIX_WIDTH: u16 = SEARCH_PREFIX.len() as u16;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let border = if focused {
        theme::accent()
    } else {
        theme::border()
    };
    let block = Block::bordered()
        .border_style(Style::new().fg(border))
        .title(Line::from(Span::styled(
            " Library ",
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [search_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)])
            .areas(inner);
    app.sidebar_viewport = list_area.height.max(1) as usize;

    let search_active = app.sidebar_search_active;
    let search_width =
        search_area.width.saturating_sub(SEARCH_PREFIX_WIDTH) as usize;
    let visible = app.sidebar_search.visible(search_width);
    let search_spans = if !search_active && app.sidebar_search.value.is_empty()
    {
        vec![
            Span::styled(SEARCH_PREFIX, Style::new().fg(theme::faint())),
            Span::styled("filter library", Style::new().fg(theme::faint())),
        ]
    } else {
        vec![
            Span::styled(
                SEARCH_PREFIX,
                Style::new().fg(if search_active {
                    theme::accent()
                } else {
                    theme::faint()
                }),
            ),
            Span::styled(
                visible.text,
                Style::new().fg(if search_active {
                    theme::text()
                } else {
                    theme::dim()
                }),
            ),
        ]
    };
    frame.render_widget(
        Paragraph::new(Line::from(search_spans)).style(if search_active {
            Style::new().bg(theme::bg_selected())
        } else {
            Style::new()
        }),
        search_area,
    );
    if search_active {
        frame.set_cursor_position((
            search_area.x + SEARCH_PREFIX_WIDTH + visible.cursor_column,
            search_area.y,
        ));
    }

    let content_width = list_area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app
        .sidebar_items
        .iter()
        .map(|item| {
            if item.header {
                return ListItem::new(Line::from(Span::styled(
                    format!(" {}", item.text),
                    Style::new()
                        .fg(theme::faint())
                        .add_modifier(Modifier::BOLD),
                )));
            }

            let active = item.filter.as_ref() == Some(&app.filter)
                || matches!(
                    (&item.filter, &app.filter),
                    (Some(Filter::Collection { id: a, .. }), Filter::Collection { id: b, .. }) if a == b
                );
            let style = if active {
                Style::new()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme::text())
            };

            let text = format!(" {}", item.text);
            let mut spans = vec![Span::styled(text.clone(), style)];
            if let Some(count) = item.count {
                let count = count.to_string();
                let used = text.width() + count.width() + 1;
                let padding = content_width.saturating_sub(used);
                spans.push(Span::raw(" ".repeat(padding)));
                spans.push(Span::styled(
                    format!("{count} "),
                    Style::new().fg(theme::faint()),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    if items.is_empty() {
        if list_area.height > 0 {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "No matches",
                    Style::new().fg(theme::faint()),
                )))
                .alignment(Alignment::Center),
                Rect::new(
                    list_area.x,
                    list_area.y + list_area.height / 2,
                    list_area.width,
                    1,
                ),
            );
        }
        return;
    }

    let list = List::new(items)
        .highlight_style(
            Style::new()
                .bg(theme::bg_selected())
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, list_area, &mut app.sidebar_state);
}
