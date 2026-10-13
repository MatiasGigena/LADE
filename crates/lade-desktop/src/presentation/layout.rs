//! Responsive composition of the workbench, decided without GTK.
//!
//! Wide (≥ 1100 logical px): header, Objectives rail, work area and inspector side by side.
//! Compact (< 1100 logical px, including 800 × 550 and 200 % scaling of a 1600 × 1100 screen):
//! the same workbench, but the secondary rails become explicit region switches — one region of
//! Objectives / work area / inspector is shown at a time below the header — and a toggle collapses
//! the Objective details above the Change Set so the code or feedback surface stays dominant.
//! Nothing is removed: every region stays reachable by the switches and by F6 / Shift+F6.

/// Window widths (logical px) below which the compact composition applies (design.md §2).
pub const COMPACT_WIDTH: i32 = 1100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LayoutClass {
    #[default]
    Wide,
    Compact,
}

impl LayoutClass {
    /// A width of zero or less means "not allocated yet" and keeps the wide composition.
    pub fn for_width(width: i32) -> Self {
        if width > 0 && width < COMPACT_WIDTH {
            Self::Compact
        } else {
            Self::Wide
        }
    }
}

/// Major regions in F6 order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Region {
    Header,
    Objectives,
    #[default]
    Main,
    Inspector,
}

pub const REGION_ORDER: [Region; 4] = [
    Region::Header,
    Region::Objectives,
    Region::Main,
    Region::Inspector,
];

impl Region {
    /// Label of the compact region switch (the header has none: it is always visible).
    pub fn switch_label(self) -> &'static str {
        match self {
            Self::Header | Self::Main => "Work area",
            Self::Objectives => "Objectives",
            Self::Inspector => "Overview · Evidence · Feedback",
        }
    }

    pub fn switch_tooltip(self) -> &'static str {
        match self {
            Self::Header | Self::Main => "Show the Workbench or Activity work area",
            Self::Objectives => "Show the Objectives list",
            Self::Inspector => "Show the Overview, Evidence and Feedback panel",
        }
    }
}

/// F6 (`forward`) / Shift+F6 target from the region holding focus (`None`: focus is elsewhere).
pub fn next_region(current: Option<Region>, forward: bool) -> Region {
    let n = REGION_ORDER.len();
    let index = match (
        current.and_then(|r| REGION_ORDER.iter().position(|o| *o == r)),
        forward,
    ) {
        (Some(i), true) => (i + 1) % n,
        (Some(i), false) => (i + n - 1) % n,
        (None, true) => 0,
        (None, false) => n - 1,
    };
    REGION_ORDER[index]
}

/// Below this width the wide composition uses its narrower rail, inspector and file-list sizes so
/// the work area keeps room between 1100 and 1280 logical pixels.
pub const ROOMY_WIDTH: i32 = 1280;

/// Preferred widths (logical px) of the side regions in the wide composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WideSizes {
    pub rail: i32,
    pub inspector: i32,
    pub file_list: i32,
}

pub fn wide_sizes(width: i32) -> WideSizes {
    if width > 0 && width < ROOMY_WIDTH {
        WideSizes {
            rail: 224,
            inspector: 280,
            file_list: 160,
        }
    } else {
        WideSizes {
            rail: 248,
            inspector: 320,
            file_list: 192,
        }
    }
}

/// What the views show for a layout class and the compact state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Composition {
    pub rail: bool,
    pub centre: bool,
    pub inspector: bool,
    /// The explicit region switches below the header.
    pub region_switches: bool,
    /// The compact header: search behind an icon with a popover, the connection status as its
    /// sign only, Keyboard shortcuts and Exit in a menu, the profile as its avatar only (it keeps
    /// its accessible name). Every header control stays reachable; nothing is clipped.
    pub compact_header: bool,
    /// Outcome, journey and version line above the Change Set.
    pub objective_details: bool,
    /// The toggle that collapses or expands those Objective details.
    pub details_toggle: bool,
}

pub fn composition(class: LayoutClass, region: Region, details_expanded: bool) -> Composition {
    match class {
        LayoutClass::Wide => Composition {
            rail: true,
            centre: true,
            inspector: true,
            region_switches: false,
            compact_header: false,
            objective_details: true,
            details_toggle: false,
        },
        LayoutClass::Compact => {
            let shown = match region {
                Region::Header => Region::Main,
                other => other,
            };
            Composition {
                rail: shown == Region::Objectives,
                centre: shown == Region::Main,
                inspector: shown == Region::Inspector,
                region_switches: true,
                compact_header: true,
                objective_details: details_expanded,
                details_toggle: true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoint_is_1100_logical_pixels() {
        assert_eq!(LayoutClass::for_width(1440), LayoutClass::Wide);
        assert_eq!(LayoutClass::for_width(1100), LayoutClass::Wide);
        assert_eq!(LayoutClass::for_width(1099), LayoutClass::Compact);
        assert_eq!(LayoutClass::for_width(800), LayoutClass::Compact);
        assert_eq!(
            LayoutClass::for_width(0),
            LayoutClass::Wide,
            "not allocated yet"
        );
    }

    #[test]
    fn wide_sizes_tighten_below_1280_within_the_design_ranges() {
        let roomy = wide_sizes(1440);
        assert_eq!(
            (roomy.rail, roomy.inspector, roomy.file_list),
            (248, 320, 192)
        );
        let tight = wide_sizes(1100);
        assert_eq!(
            (tight.rail, tight.inspector, tight.file_list),
            (224, 280, 160)
        );
        assert_eq!(wide_sizes(1280), roomy);
        // Rail 224–280 and inspector 280–360 (design.md §2).
        for size in [roomy, tight] {
            assert!((224..=280).contains(&size.rail) && (280..=360).contains(&size.inspector));
        }
    }

    #[test]
    fn wide_shows_every_region_without_switches() {
        for region in REGION_ORDER {
            let c = composition(LayoutClass::Wide, region, false);
            assert!(c.rail && c.centre && c.inspector && c.objective_details && !c.compact_header);
            assert!(!c.region_switches && !c.details_toggle);
        }
    }

    #[test]
    fn compact_shows_exactly_one_region_and_keeps_switches() {
        for region in REGION_ORDER {
            let c = composition(LayoutClass::Compact, region, false);
            let shown = [c.rail, c.centre, c.inspector];
            assert_eq!(shown.iter().filter(|s| **s).count(), 1, "{region:?}");
            assert!(c.region_switches && c.details_toggle && c.compact_header);
            assert!(!c.objective_details);
        }
        assert!(composition(LayoutClass::Compact, Region::Header, false).centre);
        assert!(composition(LayoutClass::Compact, Region::Inspector, false).inspector);
        assert!(composition(LayoutClass::Compact, Region::Main, true).objective_details);
    }

    #[test]
    fn f6_cycles_all_regions_in_both_directions() {
        let mut region = None;
        let mut seen = Vec::new();
        for _ in 0..4 {
            let next = next_region(region, true);
            seen.push(next);
            region = Some(next);
        }
        assert_eq!(seen, REGION_ORDER);
        assert_eq!(next_region(Some(Region::Header), false), Region::Inspector);
        assert_eq!(next_region(None, false), Region::Inspector);
        assert_eq!(next_region(Some(Region::Inspector), true), Region::Header);
    }
}
