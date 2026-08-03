#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ViewportSize {
    pub width: usize,
    pub height: usize,
}

impl Default for ViewportSize {
    fn default() -> Self {
        Self {
            width: 80,
            height: 1,
        }
    }
}

/// Terminal positions produced by rendering and used for mouse hit tests.
#[derive(Default)]
pub(crate) struct ScreenLayout {
    pub tab_positions: Vec<(u16, u16)>,
    pub mode_badge_pos: (u16, u16),
    pub view_badge_pos: (u16, u16),
    pub status_bar_row: u16,
    pub content_y: u16,
    pub content_height: u16,
    pub content_width: u16,
}

impl ScreenLayout {
    pub fn viewport_size(&self) -> ViewportSize {
        ViewportSize {
            width: self.content_width.max(1) as usize,
            height: self.content_height.max(1) as usize,
        }
    }
}
