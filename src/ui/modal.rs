use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::app::{
    App, ConfirmModal, InputKind, InputModal, Modal, ThemePicker,
};

use super::theme;

pub fn draw(frame: &mut Frame, app: &App) {
    match app.modal.as_ref() {
        Some(Modal::Help) => draw_help(frame, frame.area()),
        Some(Modal::Confirm(confirm)) => {
            draw_confirm(frame, frame.area(), confirm)
        }
        Some(Modal::Input(input)) => draw_input(frame, frame.area(), input),
        Some(Modal::ThemePicker(picker)) => {
            draw_theme_picker(frame, frame.area(), picker)
        }
        None => {}
    }
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let width = area.width.saturating_sub(4).min(88);
    let height = area.height.saturating_sub(2).min(29);
    let popup = centered_rect(area, width, height);
    frame.render_widget(Clear, popup);

    let block = Block::bordered()
        .border_style(Style::new().fg(theme::accent()))
        .style(Style::new().bg(theme::bg_base()))
        .title(Line::from(Span::styled(
            " Keyboard shortcuts ",
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(Line::from(Span::styled(
            " Esc / ? to close ",
            Style::new().fg(theme::faint()),
        )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let [left_area, right_area] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .areas(inner);

    let left = vec![
        section("Navigation"),
        binding("j / ↓", "Move down"),
        binding("k / ↑", "Move up"),
        binding("g / G", "First / last"),
        binding("PgUp/PgDn", "Page up / down"),
        binding("Ctrl+d/u", "Half page down / up"),
        binding("Tab", "Focus filters"),
        binding("Enter", "Open / apply"),
        binding("Esc", "Back / close"),
        Line::from(""),
        section("Bookmarks"),
        binding("Space", "Select / deselect"),
        binding("Ctrl+a", "Select all loaded bookmarks"),
        binding("/", "Search bookmarks"),
        binding("s / S", "Cycle sort forward / back"),
        binding("n", "Add bookmark"),
        binding("m", "Toggle read"),
        binding("f", "Toggle favorite"),
        binding("a", "Archive / restore"),
        binding("t", "Edit labels"),
        binding("T", "Edit title"),
        binding("E", "Edit note"),
        binding("D", "Delete selected"),
        binding("o", "Open in browser"),
        binding("r", "Refresh"),
        Line::from(""),
        Line::from(Span::styled(
            "a/f/t/m/D apply to selected items.",
            Style::new().fg(theme::dim()),
        )),
    ];

    let right = vec![
        section("Reader"),
        binding("j / k", "Scroll line"),
        binding("Space", "Next paragraph"),
        binding("PgDn / b", "Page down / up"),
        binding("g / G", "Top / bottom"),
        binding("/", "Search article"),
        binding("n / N", "Next / previous match"),
        binding("m / f / a", "Read / favorite / archive"),
        binding("t / T / E", "Labels / title / note"),
        binding("D", "Delete bookmark"),
        binding("i", "Toggle images"),
        binding("A", "Toggle text alignment"),
        binding("s", "Speed reading (Space, +/- pace)"),
        binding("o", "Open in browser"),
        binding("r", "Retry article"),
        binding("Esc", "Clear search, then back"),
        Line::from(""),
        section("General"),
        binding("?", "Toggle this help"),
        binding("Ctrl+T", "Choose color theme"),
        binding("Ctrl+C", "Quit eckly"),
        Line::from(""),
        Line::from(Span::styled(
            "Reading progress is saved when you leave the reader.",
            Style::new().fg(theme::dim()),
        )),
        Line::from(Span::styled(
            "Labels are edited as a comma separated list.",
            Style::new().fg(theme::dim()),
        )),
        Line::from(Span::styled(
            "Text fields: Ctrl+a/e start/end, Ctrl+w delete word.",
            Style::new().fg(theme::dim()),
        )),
        Line::from(Span::styled(
            "Tab completes labels in the label editor.",
            Style::new().fg(theme::dim()),
        )),
        Line::from(Span::styled(
            "In the library, / filters labels and collections.",
            Style::new().fg(theme::dim()),
        )),
        Line::from(Span::styled(
            "In the list, Esc clears search, then selection, then focuses the library.",
            Style::new().fg(theme::dim()),
        )),
    ];

    frame.render_widget(
        Paragraph::new(left).wrap(Wrap { trim: false }),
        left_area,
    );
    frame.render_widget(
        Paragraph::new(right).wrap(Wrap { trim: false }),
        right_area,
    );
}

fn draw_confirm(frame: &mut Frame, area: Rect, confirm: &ConfirmModal) {
    let width = area.width.saturating_sub(8).min(64);
    let height = 9;
    let popup = centered_rect(area, width, height);
    frame.render_widget(Clear, popup);

    let block = Block::bordered()
        .border_style(Style::new().fg(theme::red()))
        .style(Style::new().bg(theme::bg_base()))
        .title(Line::from(Span::styled(
            format!(" {} ", confirm.title),
            Style::new().fg(theme::red()).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let [message_area, hint_area] =
        Layout::vertical([Constraint::Min(2), Constraint::Length(2)])
            .areas(inner);

    frame.render_widget(
        Paragraph::new(confirm.message.as_str())
            .wrap(Wrap { trim: true })
            .style(Style::new().fg(theme::text())),
        Rect::new(
            message_area.x + 1,
            message_area.y,
            message_area.width.saturating_sub(2),
            message_area.height,
        ),
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Enter / y",
                Style::new().fg(theme::red()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" confirm   ", Style::new().fg(theme::dim())),
            Span::styled(
                "Esc / n",
                Style::new()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" cancel", Style::new().fg(theme::dim())),
        ]))
        .alignment(Alignment::Center),
        hint_area,
    );
}

fn draw_input(frame: &mut Frame, area: Rect, input: &InputModal) {
    let width = area.width.saturating_sub(8).min(76);
    let height = 7;
    let popup = centered_rect(area, width, height);
    frame.render_widget(Clear, popup);

    let block = Block::bordered()
        .border_style(Style::new().fg(theme::accent()))
        .style(Style::new().bg(theme::bg_base()))
        .title(Line::from(Span::styled(
            format!(" {} ", input.title),
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let field_area = Rect::new(
        inner.x.saturating_add(2),
        inner.y + 1,
        inner.width.saturating_sub(4),
        1,
    );
    let prompt_area = Rect::new(
        inner.x.saturating_add(2),
        inner.y,
        inner.width.saturating_sub(4),
        1,
    );
    let suggestion_area = Rect::new(
        inner.x.saturating_add(2),
        inner.y + 2,
        inner.width.saturating_sub(4),
        1,
    );
    let hint_area = Rect::new(
        inner.x.saturating_add(2),
        inner.y + 3,
        inner.width.saturating_sub(4),
        1,
    );

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            super::truncate_width(&input.prompt, prompt_area.width as usize),
            Style::new().fg(theme::dim()),
        ))),
        prompt_area,
    );

    let visible = input
        .input
        .visible(field_area.width.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("› ", Style::new().fg(theme::accent())),
            Span::styled(visible.text, Style::new().fg(theme::text())),
        ]))
        .style(Style::new().bg(theme::bg_selected())),
        field_area,
    );

    if matches!(input.kind, InputKind::EditLabels { .. }) {
        let suggestions = input
            .completion
            .as_ref()
            .map(|completion| completion.candidates.as_slice())
            .unwrap_or(input.suggestions.as_slice());
        if !suggestions.is_empty() {
            let preview = suggestions
                .iter()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    super::truncate_width(
                        &format!("Tab: {preview}"),
                        suggestion_area.width as usize,
                    ),
                    Style::new().fg(theme::faint()),
                ))),
                suggestion_area,
            );
        }
    }

    let mut hints = vec![
        Span::styled(
            "Enter",
            Style::new()
                .fg(theme::accent_alt())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" save   ", Style::new().fg(theme::dim())),
        Span::styled(
            "Esc",
            Style::new()
                .fg(theme::accent_alt())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" cancel", Style::new().fg(theme::dim())),
    ];
    if matches!(input.kind, InputKind::Search | InputKind::EditLabels { .. }) {
        hints.push(Span::styled(
            "   Ctrl+w",
            Style::new()
                .fg(theme::accent_alt())
                .add_modifier(Modifier::BOLD),
        ));
        hints.push(Span::styled(" delete word", Style::new().fg(theme::dim())));
    }
    frame.render_widget(Paragraph::new(Line::from(hints)), hint_area);

    frame.set_cursor_position((
        field_area.x + 2 + visible.cursor_column,
        field_area.y,
    ));
}

fn draw_theme_picker(frame: &mut Frame, area: Rect, picker: &ThemePicker) {
    let themes = theme::all();
    let width = area.width.saturating_sub(8).min(68);
    let height = area.height.saturating_sub(4).clamp(9, 24);
    let popup = centered_rect(area, width, height);
    frame.render_widget(Clear, popup);

    let title =
        format!(" Select theme · {}/{} ", picker.selected + 1, themes.len());
    let block = Block::bordered()
        .border_style(Style::new().fg(theme::accent()))
        .style(Style::new().bg(theme::bg_base()))
        .title(Line::from(Span::styled(
            title,
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let [list_area, hint_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)])
            .areas(inner);
    let rows = list_area.height as usize;
    let offset = if themes.len() <= rows {
        0
    } else {
        picker
            .selected
            .saturating_sub(rows / 2)
            .min(themes.len() - rows)
    };

    let lines: Vec<Line> = themes
        .iter()
        .enumerate()
        .skip(offset)
        .take(rows)
        .map(|(index, (id, name))| {
            let selected = index == picker.selected;
            let mut line = Line::from(vec![
                Span::styled(
                    if selected { "› " } else { "  " },
                    Style::new().fg(theme::accent()),
                ),
                Span::styled(
                    *name,
                    if selected {
                        Style::new()
                            .fg(theme::accent())
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(theme::text())
                    },
                ),
                Span::styled(
                    format!("  {id}"),
                    Style::new().fg(theme::faint()),
                ),
            ]);
            if selected {
                line = line.style(Style::new().bg(theme::bg_selected()));
            }
            line
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), list_area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " j/k preview · PgUp/PgDn move · Enter apply · Esc cancel",
            Style::new().fg(theme::dim()),
        ))),
        hint_area,
    );
}

fn section(title: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        title,
        Style::new()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD),
    ))
}

fn binding(key: &'static str, description: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {key:<11}"),
            Style::new().fg(theme::accent_alt()),
        ),
        Span::styled(description, Style::new().fg(theme::dim())),
    ])
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.max(10).min(area.width);
    let height = height.max(4).min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}
