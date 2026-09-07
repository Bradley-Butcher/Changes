mod layout;
mod row;
mod state;

pub use layout::DiffLayout;
pub(crate) use layout::{chunk_end, side_by_side_gutter_width, side_by_side_pane_widths};
pub use row::{RowRef, ViewKind};
pub use state::ViewportState;
