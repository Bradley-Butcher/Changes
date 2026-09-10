use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;
use std::path::Path;
use syntect::easy::HighlightLines;
use syntect::highlighting::{self, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem};
use syntect::parsing::{SyntaxReference, SyntaxSet};

const MAX_SYNTAX_CACHE_ENTRIES: usize = 128;
const MAX_HIGHLIGHT_CACHE_ENTRIES: usize = 4096;

type HighlightKey = (String, String);

/// Two-generation cache: lookups check `current` then `previous`, promoting hits. When
/// `current` fills up it becomes `previous`, so the lines on screen survive eviction
/// instead of being wiped along with everything else.
#[derive(Default)]
struct HighlightCache {
    current: HashMap<HighlightKey, Vec<CachedSpan>>,
    previous: HashMap<HighlightKey, Vec<CachedSpan>>,
}

impl HighlightCache {
    fn get(&mut self, key: &HighlightKey) -> Option<Vec<CachedSpan>> {
        if let Some(spans) = self.current.get(key) {
            return Some(spans.clone());
        }
        let spans = self.previous.remove(key)?;
        self.insert(key.clone(), spans.clone());
        Some(spans)
    }

    fn insert(&mut self, key: HighlightKey, spans: Vec<CachedSpan>) {
        if self.current.len() >= MAX_HIGHLIGHT_CACHE_ENTRIES {
            self.previous = std::mem::take(&mut self.current);
        }
        self.current.insert(key, spans);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.current.len() + self.previous.len()
    }

    fn clear(&mut self) {
        self.current.clear();
        self.previous.clear();
    }
}

pub struct Highlighter {
    syntax_set: SyntaxSet,
    theme: Theme,
    /// Maps file extensions, or extensionless file names, to syntax names.
    syntax_cache: std::cell::RefCell<HashMap<String, String>>,
    highlight_cache: std::cell::RefCell<HighlightCache>,
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
            theme: ansi_theme(),
            syntax_cache: std::cell::RefCell::new(HashMap::new()),
            highlight_cache: std::cell::RefCell::new(HighlightCache::default()),
        }
    }

    /// Drop every cached highlight. Growth is already bounded by generation eviction, and
    /// keys are content-addressed, so a diff refresh does not need this: unchanged lines
    /// simply keep hitting the cache.
    pub fn clear_highlight_cache(&self) {
        self.highlight_cache.borrow_mut().clear();
    }

    fn get_syntax(&self, file_path: &str) -> &SyntaxReference {
        let path = Path::new(file_path);
        let cache_key = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| format!("ext:{extension}"))
            .unwrap_or_else(|| {
                format!(
                    "file:{}",
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or(file_path)
                )
            });

        let cache = self.syntax_cache.borrow();
        if let Some(name) = cache.get(&cache_key)
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

        let mut cache = self.syntax_cache.borrow_mut();
        if cache.len() >= MAX_SYNTAX_CACHE_ENTRIES {
            // ponytail: whole-cache eviction is enough for this small lookup cache.
            cache.clear();
        }
        cache.insert(cache_key, syntax.name.clone());

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

        let cached = if let Some(cached) = self.highlight_cache.borrow_mut().get(&cache_key) {
            cached
        } else {
            let mut h = HighlightLines::new(syntax, &self.theme);
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
                    // The theme paints no backgrounds; the diff tints are the only ones.
                    bg: None,
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

#[cfg(test)]
mod tests {
    use super::{Highlighter, MAX_HIGHLIGHT_CACHE_ENTRIES};

    #[test]
    fn caches_stay_bounded_and_extensionless_paths_use_file_names() {
        let highlighter = Highlighter::new();
        highlighter.highlight_line_content("one", "first/Makefile", None);
        highlighter.highlight_line_content("two", "second/Dockerfile", None);
        assert_eq!(highlighter.syntax_cache.borrow().len(), 2);

        for index in 0..=MAX_HIGHLIGHT_CACHE_ENTRIES * 3 {
            highlighter.highlight_line_content(&index.to_string(), "file.rs", None);
        }
        assert!(highlighter.highlight_cache.borrow().len() <= MAX_HIGHLIGHT_CACHE_ENTRIES * 2);
    }

    #[test]
    fn recently_used_lines_survive_eviction() {
        let highlighter = Highlighter::new();
        highlighter.highlight_line_content("fn keep() {}", "file.rs", None);
        for index in 0..MAX_HIGHLIGHT_CACHE_ENTRIES {
            highlighter.highlight_line_content(&index.to_string(), "file.rs", None);
            // Touch the hot line every so often, as a visible row would be each frame.
            if index % 100 == 0 {
                highlighter.highlight_line_content("fn keep() {}", "file.rs", None);
            }
        }
        let key = ("Rust".to_string(), "fn keep() {}".to_string());
        assert!(highlighter.highlight_cache.borrow_mut().get(&key).is_some());
    }
}

#[derive(Clone)]
struct CachedSpan {
    content: String,
    fg: Color,
    bg: Option<Color>,
    modifiers: Modifier,
}

/// The terminal's own palette, encoded in syntect colours: alpha 0 marks an ANSI slot
/// held in the red channel, `DEFAULT_FG` the terminal's default foreground.
const ANSI_ALPHA: u8 = 0;
const DEFAULT_FG: u8 = 255;

const fn ansi(index: u8) -> highlighting::Color {
    highlighting::Color {
        r: index,
        g: 0,
        b: 0,
        a: ANSI_ALPHA,
    }
}

/// Syntax colours from the terminal's palette, so code reads the way it does in the
/// user's editor and shell rather than in a theme of our own. No backgrounds: the diff
/// tints supply those.
fn ansi_theme() -> Theme {
    const RED: u8 = 1;
    const GREEN: u8 = 2;
    const YELLOW: u8 = 3;
    const BLUE: u8 = 4;
    const MAGENTA: u8 = 5;
    const CYAN: u8 = 6;
    const MUTED: u8 = 245;
    let item = |scope: &str, fg: Option<u8>, font: FontStyle| ThemeItem {
        scope: scope
            .parse::<ScopeSelectors>()
            .expect("valid scope selector"),
        style: StyleModifier {
            foreground: fg.map(ansi),
            background: None,
            font_style: Some(font),
        },
    };
    let plain = FontStyle::empty();
    let mut theme = Theme::default();
    theme.settings.foreground = Some(ansi(DEFAULT_FG));
    theme.settings.background = Some(highlighting::Color::BLACK);
    theme.scopes = vec![
        item("comment", Some(MUTED), FontStyle::ITALIC),
        item("string", Some(GREEN), plain),
        item("string.regexp", Some(RED), plain),
        item(
            "constant.numeric, constant.language, constant.character",
            Some(MAGENTA),
            plain,
        ),
        item("constant.other", Some(CYAN), plain),
        item("keyword, storage", Some(BLUE), plain),
        item("keyword.control", Some(MAGENTA), plain),
        item(
            "keyword.operator, keyword.other.unit",
            Some(DEFAULT_FG),
            plain,
        ),
        item(
            "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, entity.name.namespace, entity.other.inherited-class, support.type, support.class",
            Some(CYAN),
            plain,
        ),
        item(
            "entity.name.function, support.function, support.macro, entity.name.macro",
            Some(YELLOW),
            plain,
        ),
        item(
            "meta.annotation, meta.attribute, entity.name.function.decorator, punctuation.definition.annotation",
            Some(YELLOW),
            plain,
        ),
        item("entity.name.tag", Some(BLUE), plain),
        item("entity.other.attribute-name", Some(CYAN), plain),
        item("variable.parameter", None, plain),
        item("markup.heading", Some(BLUE), FontStyle::BOLD),
        item("markup.bold", None, FontStyle::BOLD),
        item("markup.italic", None, FontStyle::ITALIC),
        item("markup.raw, markup.inline.raw", Some(GREEN), plain),
        item(
            "markup.underline.link, markup.link",
            Some(CYAN),
            FontStyle::UNDERLINE,
        ),
        item(
            "markup.list.numbered.bullet, markup.list.unnumbered.bullet, punctuation.definition.list_item",
            Some(MUTED),
            plain,
        ),
        item("markup.quote", Some(MUTED), FontStyle::ITALIC),
        item("meta.diff.header, meta.separator", Some(MUTED), plain),
        item("markup.inserted", Some(GREEN), plain),
        item("markup.deleted", Some(RED), plain),
        item("invalid", Some(RED), plain),
    ];
    theme
}

fn syntect_color_to_ratatui(c: highlighting::Color) -> Color {
    if c.a == ANSI_ALPHA {
        if c.r == DEFAULT_FG {
            Color::Reset
        } else {
            Color::Indexed(c.r)
        }
    } else {
        Color::Rgb(c.r, c.g, c.b)
    }
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
