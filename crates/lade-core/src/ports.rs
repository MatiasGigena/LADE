//! Narrow effect/storage interfaces implemented by adapters in `lade-system`.

use crate::domain::ids::{ObjectiveId, Revision, Timestamp, Uuid};
use crate::domain::objectives::Objective;
use crate::error::DomainError;

/// Wall-clock source (unix milliseconds).
pub trait Clock {
    fn now(&self) -> Timestamp;
}

/// Source of fresh identities. Adapters typically read 16 random bytes and call [`Uuid::from_random_bytes`].
pub trait IdGenerator {
    fn uuid(&mut self) -> Uuid;
}

/// Durable Objective records with optimistic concurrency.
pub trait ObjectiveStore {
    /// `NotFound` when absent.
    fn load(&self, id: ObjectiveId) -> Result<Objective, DomainError>;

    /// Stores a new Objective; `Conflict` if the id already exists.
    fn insert(&mut self, objective: &Objective) -> Result<(), DomainError>;

    /// Replaces the stored Objective only if its stored revision equals `expected` (the revision the caller
    /// loaded); otherwise `Conflict` and nothing is written.
    fn update(&mut self, objective: &Objective, expected: Revision) -> Result<(), DomainError>;
}
