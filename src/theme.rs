//! Named colors for every widget, chosen once at startup.
//!
//! Text colors are the terminal's own ANSI palette, so the app inherits whatever scheme
//! the user already picked for their shell and editor. Only the diff tints and the few
//! surfaces (header bars, popups, the status line) are fixed RGB, because no ANSI slot
//! is subtle enough for a background wash; those come in a dark and a light variant.

use ratatui::style::{Color, Modifier, Style};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind {
    Dark,
    Light,
}

impl ThemeKind {
    /// Parse `--theme` / `CHANGES_THEME`; anything unrecognised is dark.
    pub fn parse(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "light" => ThemeKind::Light,
            _ => ThemeKind::Dark,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub kind: ThemeKind,
    /// The one highlight color: active tab, mode pill, focus bar, selection, links.
    pub accent: Color,
    /// Ordinary text; `Reset` means the terminal's default foreground.
    pub text: Color,
    /// Secondary text: line numbers, hints, paths, context.
    pub muted: Color,
    /// Reserved for review warnings only.
    pub warn: Color,
    /// Review notes the user attaches to hunks.
    pub note: Color,
    pub add_fg: Color,
    pub del_fg: Color,
    /// Faint line tints; the whole line is washed, the eye reads the +/- and the emphasis.
    pub add_bg: Color,
    pub del_bg: Color,
    /// Brighter tint on the words that actually changed.
    pub add_emph: Color,
    pub del_emph: Color,
    /// Header bars, selected rows, the status line.
    pub surface: Color,
    /// A lifted surface for the selected row inside a surface.
    pub surface_raised: Color,
    pub popup_bg: Color,
    /// Brief highlight after a copy.
    pub flash: Color,
}

const DARK: Theme = Theme {
    kind: ThemeKind::Dark,
    accent: Color::Blue,
    text: Color::Reset,
    muted: Color::DarkGray,
    warn: Color::Yellow,
    note: Color::Magenta,
    add_fg: Color::Green,
    del_fg: Color::Red,
    add_bg: Color::Rgb(14, 40, 20),
    del_bg: Color::Rgb(52, 16, 18),
    add_emph: Color::Rgb(24, 84, 38),
    del_emph: Color::Rgb(118, 30, 34),
    surface: Color::Rgb(34, 37, 46),
    surface_raised: Color::Rgb(48, 52, 64),
    popup_bg: Color::Rgb(26, 28, 35),
    flash: Color::Rgb(78, 74, 28),
};

const LIGHT: Theme = Theme {
    kind: ThemeKind::Light,
    accent: Color::Blue,
    text: Color::Reset,
    muted: Color::DarkGray,
    warn: Color::Yellow,
    note: Color::Magenta,
    add_fg: Color::Green,
    del_fg: Color::Red,
    add_bg: Color::Rgb(226, 246, 228),
    del_bg: Color::Rgb(252, 228, 228),
    add_emph: Color::Rgb(170, 230, 176),
    del_emph: Color::Rgb(246, 178, 178),
    surface: Color::Rgb(232, 234, 240),
    surface_raised: Color::Rgb(214, 218, 228),
    popup_bg: Color::Rgb(246, 247, 250),
    flash: Color::Rgb(252, 240, 170),
};

static ACTIVE: OnceLock<Theme> = OnceLock::new();

/// Choose the theme for this process. Later calls are ignored; the first draw fixes it.
pub fn init(kind: ThemeKind) {
    let _ = ACTIVE.set(match kind {
        ThemeKind::Dark => DARK,
        ThemeKind::Light => LIGHT,
    });
}

/// The active theme, dark unless `init` chose otherwise.
pub fn theme() -> &'static Theme {
    ACTIVE.get_or_init(|| DARK)
}

impl Theme {
    pub fn muted_style(&self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn text_style(&self) -> Style {
        Style::default().fg(self.text)
    }

    pub fn accent_style(&self) -> Style {
        Style::default().fg(self.accent)
    }

    pub fn bold(&self) -> Style {
        Style::default().fg(self.text).add_modifier(Modifier::BOLD)
    }

    pub fn warn_style(&self) -> Style {
        Style::default().fg(self.warn).add_modifier(Modifier::BOLD)
    }

    /// The mode pill in the status line: accent background, dark text.
    pub fn pill(&self) -> Style {
        Style::default()
            .fg(match self.kind {
                ThemeKind::Dark => Color::Black,
                ThemeKind::Light => Color::White,
            })
            .bg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn popup_style(&self) -> Style {
        Style::default().bg(self.popup_bg).fg(self.text)
    }

    pub fn selection(&self) -> Style {
        Style::default()
            .bg(self.surface_raised)
            .add_modifier(Modifier::BOLD)
    }
}
