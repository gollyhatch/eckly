use std::sync::{OnceLock, RwLock};

use opaline::Theme as OpalineTheme;
use opaline::builtins;
use opaline::names::tokens;
use ratatui::style::Color;

/// The ratatui colors used by the application, resolved from opaline tokens.
#[derive(Debug, Clone, Copy)]
struct Colors {
    text: Color,
    dim: Color,
    faint: Color,
    accent: Color,
    accent_alt: Color,
    green: Color,
    red: Color,
    purple: Color,
    border: Color,
    bg_base: Color,
    bg_bar: Color,
    bg_selected: Color,
}

impl Colors {
    fn from_opaline(theme: &OpalineTheme) -> Self {
        let color = |token: &str| {
            let value = theme.color(token);
            Color::Rgb(value.r, value.g, value.b)
        };
        Self {
            text: color(tokens::TEXT_PRIMARY),
            dim: color(tokens::TEXT_SECONDARY),
            faint: color(tokens::TEXT_MUTED),
            accent: color(tokens::ACCENT_PRIMARY),
            accent_alt: color(tokens::WARNING),
            green: color(tokens::SUCCESS),
            red: color(tokens::ERROR),
            purple: color(tokens::ACCENT_TERTIARY),
            border: color(tokens::BORDER_UNFOCUSED),
            bg_base: color(tokens::BG_BASE),
            bg_bar: color(tokens::BG_PANEL),
            bg_selected: color(tokens::BG_SELECTION),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThemeInfo {
    pub id: String,
}

#[derive(Debug, Clone)]
struct Current {
    info: ThemeInfo,
    colors: Colors,
}

static CURRENT: OnceLock<RwLock<Current>> = OnceLock::new();

fn builtin_list() -> &'static [(&'static str, &'static str)] {
    builtins::builtin_names()
}

fn default_info() -> ThemeInfo {
    let (id, _) = builtin_list()
        .iter()
        .find(|(id, _)| *id == "ayu-dark")
        .copied()
        .unwrap_or(builtin_list()[0]);
    ThemeInfo { id: id.to_string() }
}

fn default_current() -> Current {
    let info = default_info();
    let theme = builtins::load_by_name(&info.id).unwrap_or_default();
    Current {
        info,
        colors: Colors::from_opaline(&theme),
    }
}

fn current() -> Current {
    CURRENT
        .get_or_init(|| RwLock::new(default_current()))
        .read()
        .expect("theme lock poisoned")
        .clone()
}

/// Resolve an ID or display name, including aliases from previous eckly themes.
fn resolve(name: &str) -> Option<(&'static str, &'static str)> {
    let normalized = name.trim().to_ascii_lowercase().replace(['_', ' '], "-");
    let normalized = match normalized.as_str() {
        "" | "default" => "ayu-dark",
        "gruvbox" => "gruvbox-dark",
        "catppuccin" => "catppuccin-mocha",
        other => other,
    };

    builtin_list().iter().copied().find(|(id, display)| {
        *id == normalized
            || display.to_ascii_lowercase().replace(['_', ' '], "-")
                == normalized
    })
}

/// Load and activate an opaline builtin theme by ID or display name.
pub fn set_name(name: &str) -> Result<ThemeInfo, String> {
    let Some((id, _)) = resolve(name) else {
        return Err(format!("unknown theme “{name}”"));
    };
    let theme = builtins::load_by_name(id)
        .ok_or_else(|| format!("could not load theme “{id}”"))?;
    let info = ThemeInfo { id: id.to_string() };
    let current = Current {
        info: info.clone(),
        colors: Colors::from_opaline(&theme),
    };
    *CURRENT
        .get_or_init(|| RwLock::new(default_current()))
        .write()
        .expect("theme lock poisoned") = current;
    Ok(info)
}

pub fn all() -> &'static [(&'static str, &'static str)] {
    builtin_list()
}

pub fn current_info() -> ThemeInfo {
    current().info
}

fn colors() -> Colors {
    CURRENT
        .get_or_init(|| RwLock::new(default_current()))
        .read()
        .expect("theme lock poisoned")
        .colors
}

pub fn text() -> Color {
    colors().text
}

pub fn dim() -> Color {
    colors().dim
}

pub fn faint() -> Color {
    colors().faint
}

pub fn accent() -> Color {
    colors().accent
}

pub fn accent_alt() -> Color {
    colors().accent_alt
}

pub fn green() -> Color {
    colors().green
}

pub fn red() -> Color {
    colors().red
}

pub fn purple() -> Color {
    colors().purple
}

pub fn border() -> Color {
    colors().border
}

pub fn bg_base() -> Color {
    colors().bg_base
}

pub fn bg_bar() -> Color {
    colors().bg_bar
}

pub fn bg_selected() -> Color {
    colors().bg_selected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_and_cycles_builtin_themes() {
        assert_eq!(
            set_name("Catppuccin Mocha").unwrap().id,
            "catppuccin-mocha"
        );
        assert_eq!(set_name("gruvbox").unwrap().id, "gruvbox-dark");
        assert!(set_name("nope").is_err());
        assert!(all().len() >= 30);

        let first = set_name("default").unwrap();
        assert_eq!(first.id, "ayu-dark");
        assert_eq!(current_info().id, first.id);
        let next = set_name("ayu-dark").unwrap();
        assert_eq!(next.id, "ayu-dark");
        assert_eq!(current_info().id, next.id);
    }
}
