//! Text that describes an Objective as a whole: its status line, its row in the rail and the
//! rows of its details section.

use super::format;
use crate::presentation::state::model::{ObjectiveDetail, ObjectiveSummary, Timestamp};

pub const DETAILS_TITLE: &str = "Details";

pub fn status_line(summary: &ObjectiveSummary) -> (&'static str, String) {
    let (sign, text) = format::status_pill(summary.lifecycle, summary.attention);
    if summary.badges.is_empty() {
        (sign, text.to_owned())
    } else {
        (sign, format!("{text} · {}", summary.badge_text()))
    }
}

pub fn objective_row_line(summary: &ObjectiveSummary) -> Option<String> {
    let (sign, text) = format::status_pill(summary.lifecycle, summary.attention);
    if !summary.badges.is_empty() {
        Some(format!("{sign} {}", summary.badge_text()))
    } else if summary.lifecycle.is_archived() {
        Some(format!("{sign} {text}"))
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailRow {
    pub key: &'static str,
    pub value: String,
    pub full: Option<String>,
    pub mono: bool,
}

pub fn details_rows(detail: &ObjectiveDetail, now: Timestamp) -> Vec<DetailRow> {
    let row = |key, value: String, full: Option<String>, mono| DetailRow {
        key,
        value,
        full,
        mono,
    };
    vec![
        row(
            "Lifecycle",
            format::lifecycle_label(detail.summary.lifecycle).to_owned(),
            None,
            false,
        ),
        row(
            "Updated",
            format::relative_time(now, detail.summary.updated_at),
            None,
            false,
        ),
        row("Branch", detail.branch.clone(), None, true),
        row(
            "Started at",
            detail.start_commit.short().to_owned(),
            Some(detail.start_commit.hex.clone()),
            true,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::state::model::{Badge, Lifecycle};
    use crate::presentation::test_fixture;

    #[test]
    fn the_list_row_says_only_what_its_group_does_not() {
        let base = test_fixture::details().into_iter().next().unwrap();
        let mut s = base.summary.clone();
        s.badges = vec![Badge::VerificationFailed, Badge::UnsentDrafts];
        let line = objective_row_line(&s).unwrap();
        assert!(line.contains("check failed") && !line.contains('\n'));
        let (_, attention) = format::status_pill(s.lifecycle, s.attention);
        assert!(!line.contains(attention));
        assert!(status_line(&s).1.contains(" · "));
        s.badges.clear();
        s.lifecycle = Lifecycle::Active;
        assert!(objective_row_line(&s).is_none());
        s.lifecycle = Lifecycle::Integrated;
        assert!(objective_row_line(&s).unwrap().contains("Integrated"));
    }

    #[test]
    fn details_hold_only_what_no_other_surface_shows() {
        let d = test_fixture::details()
            .into_iter()
            .find(|d| d.session.is_some())
            .unwrap();
        let rows = details_rows(&d, d.summary.updated_at);
        let keys: Vec<&str> = rows.iter().map(|r| r.key).collect();
        assert_eq!(keys, ["Lifecycle", "Updated", "Branch", "Started at"]);
        let start = rows.iter().find(|r| r.key == "Started at").unwrap();
        assert_eq!(start.full.as_deref(), Some(d.start_commit.hex.as_str()));
    }
}
