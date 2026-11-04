//! The view model the store holds: plain data converted from the service protocol, grouped by
//! what it describes.

mod activity;
mod ids;
mod objective;
mod review;
mod service;

pub use activity::*;
pub use ids::*;
pub use objective::*;
pub use review::*;
pub use service::*;
