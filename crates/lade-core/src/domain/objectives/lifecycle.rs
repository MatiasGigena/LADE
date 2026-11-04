//! The Objective lifecycle and its allowed transitions.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lifecycle {
    Defined,
    Active,
    Frozen,
    Approved,
    Integrated,
    CompletedNoChanges,
    Discarded,
}

impl Lifecycle {
    pub const ALL: [Self; 7] = [
        Self::Defined,
        Self::Active,
        Self::Frozen,
        Self::Approved,
        Self::Integrated,
        Self::CompletedNoChanges,
        Self::Discarded,
    ];

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Integrated | Self::CompletedNoChanges | Self::Discarded
        )
    }

    pub const fn permits(self, to: Self) -> bool {
        use Lifecycle::{
            Active, Approved, CompletedNoChanges, Defined, Discarded, Frozen, Integrated,
        };
        matches!(
            (self, to),
            (Defined, Active)
                | (Active, Frozen)
                | (Frozen, Active)
                | (Frozen, Approved)
                | (Approved, Frozen)
                | (Approved, Integrated)
                | (Defined | Active | Frozen, CompletedNoChanges)
                | (Defined | Active | Frozen | Approved, Discarded)
        )
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::Defined => "defined",
            Self::Active => "active",
            Self::Frozen => "frozen",
            Self::Approved => "approved",
            Self::Integrated => "integrated",
            Self::CompletedNoChanges => "completed_no_changes",
            Self::Discarded => "discarded",
        }
    }
}
