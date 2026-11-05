//! The parts of [`App`](super::App) that run the window as a whole: its frame and layout,
//! keyboard shortcuts and focus moves, and closing or exiting.

mod closing;
mod frame;
mod shortcuts;

pub(super) use closing::CloseAfter;
