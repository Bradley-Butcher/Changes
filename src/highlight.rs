use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;
use syntect::easy::HighlightLines;
use syntect::highlighting::{self, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

pub struct Highlighter {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    /// Maps file extensions to syntax name to avoid repeated find_syntax_for_file calls
    syntax_cache: std::cell::RefCell<HashMap<String, String>>,
    highlight_cache: std::cell::RefCell<HashMap<(String, String), Vec<CachedSpan>>>,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            syntax_cache: std::cell::RefCell::new(HashMap::new()),
            highlight_cache: std::cell::RefCell::new(HashMap::new()),
        }
    }

    /// Clear the highlight cache. Call when diff content changes to prevent unbounded growth.
    /// The syntax cache (ext -> syntax name) is kept — it's bounded by extension count.
    pub fn clear_highlight_cache(&self) {
        self.highlight_cache.borrow_mut().clear();
    }

    fn get_syntax(&self, file_path: &str) -> &SyntaxReference {
        // Extract extension for cache key
        let ext = file_path.rsplit('.').next().unwrap_or("").to_string();

        let cache = self.syntax_cache.borrow();
        if let Some(name) = cache.get(&ext)
            && let Some(syn) = self.syntax_set.find_syntax_by_name(name)
        {
            return syn;
        }
        drop(cache);

        let syntax = self
            .syntax_set
            .find_syntax_for_file(file_path)
            .ok()
            .flatten()
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        self.syntax_cache
            .borrow_mut()
            .insert(ext, syntax.name.clone());

        syntax
    }

    pub fn highlight_line_content<'a>(
        &self,
        text: &str,
        file_path: &str,
        bg_override: Option<Color>,
    ) -> Line<'a> {
        let syntax = self.get_syntax(file_path);
        let cache_key = (syntax.name.clone(), text.to_string());

        let cached = if let Some(cached) = self.highlight_cache.borrow().get(&cache_key) {
            cached.clone()
        } else {
            let theme = &self.theme_set.themes["base16-ocean.dark"];
            let mut h = HighlightLines::new(syntax, theme);
            let regions = match h.highlight_line(text, &self.syntax_set) {
                Ok(regions) => regions,
                Err(_) => {
                    return Line::from(text.to_string());
                }
            };

            let cached: Vec<CachedSpan> = regions
                .into_iter()
                .map(|(style, content)| CachedSpan {
                    content: content.to_string(),
                    fg: syntect_color_to_ratatui(style.foreground),
                    bg: if style.background
                        != (highlighting::Color {
                            r: 0,
                            g: 0,
                            b: 0,
                            a: 0,
                        }) {
                        Some(syntect_color_to_ratatui(style.background))
                    } else {
                        None
                    },
                    modifiers: syntect_modifiers(style.font_style),
                })
                .collect();
            self.highlight_cache
                .borrow_mut()
                .insert(cache_key, cached.clone());
            cached
        };

        let spans: Vec<Span<'a>> = cached
            .into_iter()
            .map(|span| {
                let mut style = Style::default().fg(span.fg).add_modifier(span.modifiers);
                if let Some(bg) = bg_override.or(span.bg) {
                    style = style.bg(bg);
                }
                Span::styled(span.content, style)
            })
            .collect();

        Line::from(spans)
    }
}

#[derive(Clone)]
struct CachedSpan {
    content: String,
    fg: Color,
    bg: Option<Color>,
    modifiers: Modifier,
}

fn syntect_color_to_ratatui(c: highlighting::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

fn syntect_modifiers(font_style: highlighting::FontStyle) -> Modifier {
    let mut modifiers = Modifier::empty();
    if font_style.contains(highlighting::FontStyle::BOLD) {
        modifiers |= Modifier::BOLD;
    }
    if font_style.contains(highlighting::FontStyle::ITALIC) {
        modifiers |= Modifier::ITALIC;
    }
    modifiers
}
