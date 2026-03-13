use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{self, ThemeSet};
use syntect::parsing::SyntaxSet;

pub struct Highlighter {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
        }
    }

    pub fn highlight_line_content<'a>(
        &self,
        text: &str,
        file_path: &str,
        bg_override: Option<Color>,
    ) -> Line<'a> {
        let syntax = self
            .syntax_set
            .find_syntax_for_file(file_path)
            .ok()
            .flatten()
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let theme = &self.theme_set.themes["base16-ocean.dark"];
        let mut h = HighlightLines::new(syntax, theme);

        let regions = match h.highlight_line(text, &self.syntax_set) {
            Ok(regions) => regions,
            Err(_) => {
                return Line::from(text.to_string());
            }
        };

        let spans: Vec<Span<'a>> = regions
            .into_iter()
            .map(|(style, content)| {
                let fg = syntect_color_to_ratatui(style.foreground);
                let mut ratatui_style = Style::default().fg(fg);
                if let Some(bg) = bg_override {
                    ratatui_style = ratatui_style.bg(bg);
                } else if style.background != (highlighting::Color { r: 0, g: 0, b: 0, a: 0 }) {
                    ratatui_style = ratatui_style.bg(syntect_color_to_ratatui(style.background));
                }
                if style.font_style.contains(highlighting::FontStyle::BOLD) {
                    ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
                }
                if style.font_style.contains(highlighting::FontStyle::ITALIC) {
                    ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
                }
                Span::styled(content.to_string(), ratatui_style)
            })
            .collect();

        Line::from(spans)
    }
}

fn syntect_color_to_ratatui(c: highlighting::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}
