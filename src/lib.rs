pub mod app;
pub mod diff;
pub mod git;
pub mod highlight;
pub mod outline;
pub mod runtime;
pub mod symbols;
pub mod ui;
pub mod viewport;
pub(crate) mod watcher;

pub use runtime::run;
