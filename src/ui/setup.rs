use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::app::{App, OAuthState, SetupField};

use super::theme;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if let Some(oauth) = &app.oauth {
        draw_oauth(frame, area, oauth, app.tick);
        return;
    }
    let width = area.width.saturating_sub(8).clamp(32, 68);
    let height = area.height.saturating_sub(4).min(16);

    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, box_area, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width),
        Constraint::Fill(1),
    ])
    .areas(middle);

    let block = Block::bordered()
        .border_style(Style::new().fg(theme::accent()))
        .style(Style::new().bg(theme::bg_base()))
        .title(Line::from(Span::styled(
            " Welcome to eckly ",
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let Some(setup) = &app.setup else {
        return;
    };

    let content = Rect::new(
        inner.x.saturating_add(2),
        inner.y,
        inner.width.saturating_sub(4),
        inner.height,
    );

    let mut row = 0;
    draw_row(
        frame,
        content,
        &mut row,
        Line::from(Span::styled(
            "Read and manage your Readeck bookmarks from the terminal.",
            Style::new().fg(theme::text()),
        )),
    );
    draw_row(
        frame,
        content,
        &mut row,
        Line::from(Span::styled(
            "Connect to your instance with a Readeck API token.",
            Style::new().fg(theme::dim()),
        )),
    );
    row += 1;

    draw_row(
        frame,
        content,
        &mut row,
        Line::from(Span::styled(
            "SERVER URL",
            Style::new().fg(theme::faint()).add_modifier(Modifier::BOLD),
        )),
    );
    let cursor = draw_field(
        frame,
        content,
        &mut row,
        &setup.server,
        setup.focus == SetupField::Server,
        "https://readeck.example.com",
        setup.busy,
    );
    row += 1;

    draw_row(
        frame,
        content,
        &mut row,
        Line::from(Span::styled(
            "API TOKEN",
            Style::new().fg(theme::faint()).add_modifier(Modifier::BOLD),
        )),
    );
    let token_cursor = draw_field(
        frame,
        content,
        &mut row,
        &setup.token,
        setup.focus == SetupField::Token,
        "paste your API token",
        setup.busy,
    );
    row += 1;

    if let Some(error) = &setup.error {
        draw_row(
            frame,
            content,
            &mut row,
            Line::from(Span::styled(
                super::truncate_width(error, content.width as usize),
                Style::new().fg(theme::red()),
            )),
        );
    } else {
        draw_row(
            frame,
            content,
            &mut row,
            Line::from(Span::styled(
                "Create a token in Readeck under Settings → API tokens.",
                Style::new().fg(theme::faint()),
            )),
        );
    }

    row += 1;
    if setup.busy {
        draw_row(
            frame,
            content,
            &mut row,
            Line::from(Span::styled(
                format!("{}  Connecting to Readeck…", super::spinner(app.tick)),
                Style::new().fg(theme::accent_alt()),
            )),
        );
    } else {
        draw_row(
            frame,
            content,
            &mut row,
            Line::from(vec![
                Span::styled(
                    "Enter",
                    Style::new()
                        .fg(theme::accent_alt())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" connect   ", Style::new().fg(theme::dim())),
                Span::styled(
                    "Tab",
                    Style::new()
                        .fg(theme::accent_alt())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" switch   ", Style::new().fg(theme::dim())),
                Span::styled(
                    "Ctrl+O",
                    Style::new()
                        .fg(theme::accent_alt())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" OAuth   ", Style::new().fg(theme::dim())),
                Span::styled(
                    "Esc",
                    Style::new()
                        .fg(theme::accent_alt())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" quit", Style::new().fg(theme::dim())),
            ]),
        );
    }

    if let Some((x, y)) = cursor.or(token_cursor) {
        if !setup.busy {
            frame.set_cursor_position((x, y));
        }
    }
}

fn draw_oauth(frame: &mut Frame, area: Rect, oauth: &OAuthState, tick: u64) {
    let width = area.width.saturating_sub(8).clamp(34, 76);
    let height = area.height.saturating_sub(4).clamp(10, 19);
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, popup, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width),
        Constraint::Fill(1),
    ])
    .areas(middle);

    let block = Block::bordered()
        .border_style(Style::new().fg(theme::accent()))
        .style(Style::new().bg(theme::bg_base()))
        .title(Line::from(Span::styled(
            " OAuth sign-in ",
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let content = Rect::new(
        inner.x.saturating_add(2),
        inner.y.saturating_add(1),
        inner.width.saturating_sub(4),
        inner.height.saturating_sub(2),
    );

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Server  ", Style::new().fg(theme::faint())),
            Span::styled(
                super::truncate_width(&oauth.server, content.width as usize),
                Style::new().fg(theme::dim()),
            ),
        ]),
        Line::from(""),
    ];

    if let Some(error) = &oauth.error {
        lines.push(Line::from(Span::styled(
            "Authorization failed",
            Style::new().fg(theme::red()).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::new().fg(theme::text()),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Press Esc to return to setup.",
            Style::new().fg(theme::faint()),
        )));
    } else if let Some(device) = &oauth.device {
        lines.push(Line::from(Span::styled(
            "Open this URL in your browser:",
            Style::new().fg(theme::text()),
        )));
        lines.push(Line::from(Span::styled(
            device.verification_uri.clone(),
            Style::new().fg(theme::accent()),
        )));
        if let Some(complete) = &device.verification_uri_complete {
            if complete != &device.verification_uri {
                lines.push(Line::from(Span::styled(
                    "Or open the complete link:",
                    Style::new().fg(theme::dim()),
                )));
                lines.push(Line::from(Span::styled(
                    complete.clone(),
                    Style::new().fg(theme::dim()),
                )));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Code  ", Style::new().fg(theme::faint())),
            Span::styled(
                format_user_code(&device.user_code),
                Style::new()
                    .fg(theme::accent_alt())
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("{}  Waiting for approval…", super::spinner(tick)),
            Style::new().fg(theme::dim()),
        )));
        lines.push(Line::from(Span::styled(
            "Press Esc to cancel.",
            Style::new().fg(theme::faint()),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("{}  Requesting a device code…", super::spinner(tick)),
            Style::new().fg(theme::dim()),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Press Esc to cancel.",
            Style::new().fg(theme::faint()),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        content,
    );
}

fn format_user_code(code: &str) -> String {
    if code.chars().count() > 4 {
        let mut chars = code.chars();
        let head: String = chars.by_ref().take(4).collect();
        let tail: String = chars.collect();
        format!("{head}-{tail}")
    } else {
        code.to_string()
    }
}

fn draw_row(frame: &mut Frame, area: Rect, row: &mut u16, line: Line<'static>) {
    if *row >= area.height {
        return;
    }
    let rect = Rect::new(area.x, area.y + *row, area.width, 1);
    frame.render_widget(Paragraph::new(line), rect);
    *row += 1;
}

#[allow(clippy::too_many_arguments)]
fn draw_field(
    frame: &mut Frame,
    area: Rect,
    row: &mut u16,
    input: &crate::input::TextInput,
    focused: bool,
    placeholder: &str,
    busy: bool,
) -> Option<(u16, u16)> {
    if *row >= area.height {
        return None;
    }
    let rect = Rect::new(area.x, area.y + *row, area.width, 1);
    let visible = input.visible(area.width.saturating_sub(2) as usize);

    let prefix_style = if focused {
        Style::new().fg(theme::accent())
    } else {
        Style::new().fg(theme::faint())
    };
    let text_style = if focused {
        Style::new().fg(theme::text()).bg(theme::bg_selected())
    } else {
        Style::new().fg(theme::text())
    };
    let spans = if visible.text.is_empty() {
        vec![
            Span::styled(if focused { "› " } else { "  " }, prefix_style),
            Span::styled(
                super::truncate_width(
                    placeholder,
                    area.width.saturating_sub(2) as usize,
                ),
                Style::new().fg(theme::faint()).bg(if focused {
                    theme::bg_selected()
                } else {
                    theme::bg_bar()
                }),
            ),
        ]
    } else {
        vec![
            Span::styled(if focused { "› " } else { "  " }, prefix_style),
            Span::styled(visible.text, text_style),
        ]
    };
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(if focused {
            Style::new().bg(theme::bg_selected())
        } else {
            Style::new()
        }),
        rect,
    );

    *row += 1;
    if focused && !busy {
        Some((rect.x + 2 + visible.cursor_column, rect.y))
    } else {
        None
    }
}
