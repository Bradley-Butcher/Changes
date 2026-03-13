use std::ops::Range;

const DEFAULT_OVERSCAN: usize = 8;

#[derive(Debug, Clone)]
pub struct ViewportState {
    scroll_offset: usize,
    overscan: usize,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            overscan: DEFAULT_OVERSCAN,
        }
    }
}

impl ViewportState {
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn clamp_scroll(&mut self, total_lines: usize, viewport_height: usize) {
        self.scroll_offset = self.clamped_offset(self.scroll_offset, total_lines, viewport_height);
    }

    pub fn visible_range(&self, total_lines: usize, viewport_height: usize) -> Range<usize> {
        let start = self.clamped_offset(self.scroll_offset, total_lines, viewport_height);
        let end = (start + viewport_height).min(total_lines);
        start..end
    }

    pub fn warm_range(&self, total_lines: usize, viewport_height: usize) -> Range<usize> {
        let visible = self.visible_range(total_lines, viewport_height);
        let start = visible.start.saturating_sub(self.overscan);
        let end = (visible.end + self.overscan).min(total_lines);
        start..end
    }

    pub fn scroll_by(&mut self, delta: isize, total_lines: usize, viewport_height: usize) {
        let next = self.scroll_offset.saturating_add_signed(delta);
        self.scroll_offset = self.clamped_offset(next, total_lines, viewport_height);
    }

    pub fn jump_to(&mut self, row: usize, total_lines: usize, viewport_height: usize) {
        self.scroll_offset = self.clamped_offset(row, total_lines, viewport_height);
    }

    pub fn jump_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn jump_to_bottom(&mut self, total_lines: usize, viewport_height: usize) {
        self.scroll_offset = self.max_offset(total_lines, viewport_height);
    }

    fn clamped_offset(
        &self,
        requested: usize,
        total_lines: usize,
        viewport_height: usize,
    ) -> usize {
        requested.min(self.max_offset(total_lines, viewport_height))
    }

    fn max_offset(&self, total_lines: usize, viewport_height: usize) -> usize {
        total_lines.saturating_sub(viewport_height.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::ViewportState;

    #[test]
    fn visible_range_clamps_to_last_page() {
        let mut viewport = ViewportState::default();
        viewport.jump_to_bottom(100, 20);
        assert_eq!(viewport.visible_range(100, 20), 80..100);
    }

    #[test]
    fn scroll_by_clamps_at_zero() {
        let mut viewport = ViewportState::default();
        viewport.scroll_by(-10, 50, 10);
        assert_eq!(viewport.scroll_offset(), 0);
    }
}
