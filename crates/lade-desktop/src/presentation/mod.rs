//! GTK-free presentation: the state store, the controller, text formatting and the layout,
//! keybinding and terminal rules views apply.

pub mod controller;
pub mod keys;
pub mod layout;
pub mod objective_actions;
pub mod state;
pub mod terminal;
#[cfg(test)]
pub mod test_fixture;
pub mod text;
