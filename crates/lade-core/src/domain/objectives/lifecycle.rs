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

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Integrated | Self::CompletedNoChanges | Self::Discarded
        )
    }

    #[must_use]
    pub const fn permits(self, to: Self) -> bool {
        use Lifecycle::{
            Active, Approved, CompletedNoChanges, Defined, Discarded, Frozen, Integrated,
        };
        matches!(
            (self, to),
            (Defined, Active)
                | (Active, Frozen)
                | (Frozen, Active | Approved)
                | (Approved, Frozen | Integrated)
                | (Defined | Active | Frozen, CompletedNoChanges)
                | (Defined | Active | Frozen | Approved, Discarded)
        )
    }

    #[must_use]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_table_is_exact() {
        use Lifecycle::*;
        let allowed = [
            (Defined, Active),
            (Active, Frozen),
            (Frozen, Active),
            (Frozen, Approved),
            (Approved, Frozen),
            (Approved, Integrated),
            (Defined, CompletedNoChanges),
            (Active, CompletedNoChanges),
            (Frozen, CompletedNoChanges),
            (Defined, Discarded),
            (Active, Discarded),
            (Frozen, Discarded),
            (Approved, Discarded),
        ];
        for from in Lifecycle::ALL {
            for to in Lifecycle::ALL {
                assert_eq!(
                    from.permits(to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
        for terminal in [Integrated, CompletedNoChanges, Discarded] {
            assert!(terminal.is_terminal());
            assert!(Lifecycle::ALL.iter().all(|to| !terminal.permits(*to)));
        }
    }

    #[test]
    fn every_non_terminal_state_can_still_be_discarded_or_reach_a_terminal_state() {
        for from in Lifecycle::ALL.iter().filter(|state| !state.is_terminal()) {
            assert!(from.permits(Lifecycle::Discarded), "{from:?}");
            assert!(!from.permits(*from), "{from:?} never loops onto itself");
        }
        let terminal: Vec<Lifecycle> = Lifecycle::ALL
            .into_iter()
            .filter(|state| state.is_terminal())
            .collect();
        assert_eq!(
            terminal,
            [
                Lifecycle::Integrated,
                Lifecycle::CompletedNoChanges,
                Lifecycle::Discarded
            ]
        );
        assert!(
            !Lifecycle::Approved.permits(Lifecycle::CompletedNoChanges),
            "an approved candidate has changes"
        );
    }

    #[test]
    fn codes_are_distinct_snake_case_names() {
        let codes: Vec<&str> = Lifecycle::ALL.iter().map(|state| state.code()).collect();
        assert_eq!(
            codes,
            [
                "defined",
                "active",
                "frozen",
                "approved",
                "integrated",
                "completed_no_changes",
                "discarded"
            ]
        );
    }
}
