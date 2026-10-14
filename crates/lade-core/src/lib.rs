pub mod application;
pub mod domain;
pub mod error;
pub mod ports;

#[cfg(test)]
mod testing;

pub use domain::ids::Revision;
pub use error::DomainError;
