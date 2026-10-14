use crate::domain::ids::{ObjectiveId, Revision, Timestamp, Uuid};
use crate::domain::objectives::Objective;
use crate::error::DomainError;

pub trait Clock {
    fn now(&self) -> Timestamp;
}

pub trait IdGenerator {
    fn uuid(&mut self) -> Uuid;
}

pub trait ObjectiveStore {
    fn load(&self, id: ObjectiveId) -> Result<Objective, DomainError>;

    fn insert(&mut self, objective: &Objective) -> Result<(), DomainError>;

    fn update(&mut self, objective: &Objective, expected: Revision) -> Result<(), DomainError>;
}
