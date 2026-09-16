//! Platform-free domain boundary. No lifecycle or execution is implemented in IMP-001.

/// Domain revisions are distinct from their decimal-string wire representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Revision(u64);

impl Revision {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    /// Exhaustion must reject a transition, never wrap to an earlier revision.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::Revision;

    #[test]
    fn revisions_advance_without_wrapping() {
        assert_eq!(Revision::new(0).next(), Some(Revision::new(1)));
        assert_eq!(Revision::new(u64::MAX).next(), None);
    }
}
