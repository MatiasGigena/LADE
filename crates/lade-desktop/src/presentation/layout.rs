pub const COMPACT_WIDTH: i32 = 1100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LayoutClass {
    #[default]
    Wide,
    Compact,
}

impl LayoutClass {
    pub fn for_width(width: i32) -> Self {
        if width > 0 && width < COMPACT_WIDTH {
            Self::Compact
        } else {
            Self::Wide
        }
    }
}

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

pub const ROOMY_WIDTH: i32 = 1280;

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
            rail: 264,
            inspector: 344,
            file_list: 192,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Composition {
    pub rail: bool,
    pub centre: bool,
    pub inspector: bool,
    pub region_switches: bool,
    pub compact_header: bool,
    pub objective_details: bool,
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
            (264, 344, 192)
        );
        let tight = wide_sizes(1100);
        assert_eq!(
            (tight.rail, tight.inspector, tight.file_list),
            (224, 280, 160)
        );
        assert_eq!(wide_sizes(1280), roomy);
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
