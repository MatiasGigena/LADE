use crate::presentation::state::model::{
    ActionAvailability, ActionName, ActivityRole, ApprovalState, Attention, Basis, CandidateStatus,
    ChangeStatus, DeliveryMode, DeliveryState, Execution, ExecutionState, InputSummary, Lifecycle,
    ObjectiveDetail, Reference, ReferenceVersion, SearchScope, Side, Timestamp, VerificationState,
    VerificationSummary,
};

pub fn attention_label(attention: Attention) -> &'static str {
    match attention {
        Attention::NeedsAttention => "Needs attention",
        Attention::ReadyForReview => "Ready for review",
        Attention::Working => "Working",
        Attention::Idle => "Idle",
        Attention::Archived => "Archived",
    }
}

pub fn attention_sign(attention: Attention) -> &'static str {
    match attention {
        Attention::NeedsAttention => "!",
        Attention::ReadyForReview => "◎",
        Attention::Working => "↻",
        Attention::Idle => "○",
        Attention::Archived => "–",
    }
}

pub fn status_pill(lifecycle: Lifecycle, attention: Attention) -> (&'static str, &'static str) {
    match lifecycle {
        Lifecycle::Integrated => ("✓", "Integrated"),
        Lifecycle::CompletedNoChanges => ("✓", "Completed, no changes"),
        Lifecycle::Discarded => ("✕", "Discarded"),
        _ => (attention_sign(attention), attention_label(attention)),
    }
}

pub fn lifecycle_label(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Defined => "Defined",
        Lifecycle::Active => "Active",
        Lifecycle::Frozen => "Frozen for review",
        Lifecycle::Approved => "Approved",
        Lifecycle::Integrated => "Integrated",
        Lifecycle::CompletedNoChanges => "Completed without changes",
        Lifecycle::Discarded => "Discarded",
    }
}

pub fn execution_label(execution: &Execution) -> String {
    match execution.state {
        ExecutionState::None => "Not started".to_owned(),
        ExecutionState::Starting => "Starting".to_owned(),
        ExecutionState::Running => "Running".to_owned(),
        ExecutionState::Idle => "Idle — waiting for input".to_owned(),
        ExecutionState::Stopping => "Stopping".to_owned(),
        ExecutionState::Cancelled => "Cancelled".to_owned(),
        ExecutionState::Failed => match &execution.reason {
            Some(reason) => format!("Failed — {reason}"),
            None => "Failed".to_owned(),
        },
    }
}

pub fn execution_label_for(detail: &ObjectiveDetail) -> String {
    let label = execution_label(&detail.summary.execution);
    if detail.availability(ActionName::ExecutionResume) == ActionAvailability::Enabled {
        format!("{label} — resumable")
    } else {
        label
    }
}

pub fn relative_time(now: Timestamp, then: Timestamp) -> String {
    let seconds = (now.0 - then.0).max(0) / 1000;
    match seconds {
        0..=44 => "just now".to_owned(),
        45..=89 => "1 min ago".to_owned(),
        90..=3_599 => format!("{} min ago", (seconds + 30) / 60),
        3_600..=86_399 => {
            let hours = seconds / 3_600;
            if hours == 1 {
                "1 hour ago".to_owned()
            } else {
                format!("{hours} hours ago")
            }
        }
        _ => {
            let days = seconds / 86_400;
            if days == 1 {
                "1 day ago".to_owned()
            } else {
                format!("{days} days ago")
            }
        }
    }
}

pub fn duration_label(start: Timestamp, end: Option<Timestamp>) -> String {
    let Some(end) = end else {
        return "running".to_owned();
    };
    let ms = (end.0 - start.0).max(0);
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1_000.0)
    } else {
        format!("{} min {} s", ms / 60_000, (ms % 60_000) / 1_000)
    }
}

pub fn byte_size(bytes: u64) -> String {
    const UNITS: [&str; 3] = ["KiB", "MiB", "GiB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

pub fn file_status(status: ChangeStatus) -> (&'static str, &'static str) {
    match status {
        ChangeStatus::Added => ("A", "Added"),
        ChangeStatus::Modified => ("M", "Modified"),
        ChangeStatus::Deleted => ("D", "Deleted"),
        ChangeStatus::Renamed => ("R", "Renamed"),
        ChangeStatus::TypeChanged => ("T", "Type changed"),
    }
}

pub fn verification_result(state: VerificationState) -> (&'static str, &'static str) {
    match state {
        VerificationState::Running => ("…", "Running"),
        VerificationState::Passed => ("✓", "Passed"),
        VerificationState::Failed => ("✕", "Failed"),
        VerificationState::Interrupted => ("!", "Interrupted"),
        VerificationState::Error => ("!", "Error"),
    }
}

pub fn delivery_label(delivery: DeliveryState, blocked_reason: Option<&str>) -> String {
    match delivery {
        DeliveryState::Queued => "Accepted · queued".to_owned(),
        DeliveryState::Dispatched => "Accepted · dispatched".to_owned(),
        DeliveryState::Received => "Accepted · received by agent".to_owned(),
        DeliveryState::Uncertain => "Accepted · delivery uncertain — not resent".to_owned(),
        DeliveryState::Blocked => match blocked_reason {
            Some(reason) => format!("Accepted · blocked. {}", sentence(reason)),
            None => "Accepted · blocked".to_owned(),
        },
    }
}

/// `text` as a sentence: trimmed, first letter capitalized, ending with `.`, `!` or `?`.
pub fn sentence(text: &str) -> String {
    let text = text.trim();
    let mut chars = text.chars();
    let mut out: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => return String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

pub fn mode_label(mode: DeliveryMode) -> &'static str {
    match mode {
        DeliveryMode::Prompt => "Prompt",
        DeliveryMode::Steer => "Steer",
        DeliveryMode::FollowUp => "Follow-up",
    }
}

pub fn mode_explanation(mode: DeliveryMode) -> &'static str {
    match mode {
        DeliveryMode::Prompt => "Starts a turn when the agent is idle.",
        DeliveryMode::Steer => "Delivered after the current tools finish; not an instant stop.",
        DeliveryMode::FollowUp => "Delivered after the agent finishes its current work.",
    }
}

pub fn role_label(role: ActivityRole, tool_name: Option<&str>) -> String {
    match role {
        ActivityRole::User => "User".to_owned(),
        ActivityRole::Assistant => "Agent".to_owned(),
        ActivityRole::Thinking => "Thinking".to_owned(),
        ActivityRole::Tool => format!("Tool {}", tool_name.unwrap_or("call")),
        ActivityRole::System => "System".to_owned(),
    }
}

pub fn candidate_status(status: CandidateStatus) -> &'static str {
    match status {
        CandidateStatus::Current => "current",
        CandidateStatus::Historical => "historical",
        CandidateStatus::Invalidated => "invalidated",
    }
}

pub fn approval_label(state: ApprovalState) -> &'static str {
    match state {
        ApprovalState::Valid => "Approval valid for the current candidate and target",
        ApprovalState::Consumed => "Approval consumed by integration",
        ApprovalState::Invalidated => "Approval invalidated — the candidate or target changed",
    }
}

pub fn scope_label(scope: SearchScope) -> &'static str {
    match scope {
        SearchScope::Objectives => "Objectives",
        SearchScope::Files => "Files",
        SearchScope::Content => "Content",
    }
}

pub fn side_label(side: Side) -> &'static str {
    match side {
        Side::Old => "old side",
        Side::New => "new side",
    }
}

pub fn basis_label(basis: &Basis, detail: Option<&ObjectiveDetail>) -> String {
    let ordinal = |id: &crate::presentation::state::model::CandidateId| {
        detail
            .and_then(|d| d.candidates.iter().find(|c| &c.candidate_id == id))
            .map_or_else(|| id.short().to_owned(), |c| c.ordinal.to_string())
    };
    match basis {
        Basis::Working => "Working inspection".to_owned(),
        Basis::Candidate { candidate_id } => format!("Candidate {}", ordinal(candidate_id)),
        Basis::Since {
            from_candidate_id,
            to_candidate_id,
        } => format!(
            "Since previous review (Candidate {} → {})",
            ordinal(from_candidate_id),
            ordinal(to_candidate_id)
        ),
    }
}

pub fn version_line(
    basis: &Basis,
    service_label: &str,
    candidate_status: Option<CandidateStatus>,
    target: &crate::presentation::state::model::TargetRef,
) -> (String, String) {
    let short_target = target.label();
    let full_target = format!("{} at {}", target.branch, target.commit.hex);
    match basis {
        Basis::Working => (
            format!("Captured · may change · vs {short_target}"),
            format!(
                "{service_label}: working capture of ongoing work — may change. Compared with {full_target}."
            ),
        ),
        Basis::Candidate { .. } => (
            format!("Frozen · vs {short_target}"),
            format!(
                "{service_label}: frozen candidate ({}). Compared with {full_target}.",
                candidate_status.map_or("frozen", self::candidate_status)
            ),
        ),
        Basis::Since { .. } => (
            format!("Revision delta · vs {short_target}"),
            format!("{service_label}: comparing two frozen candidates. Target {full_target}."),
        ),
    }
}

pub fn reference_version_label(
    version: &ReferenceVersion,
    detail: Option<&ObjectiveDetail>,
) -> String {
    match version {
        ReferenceVersion::Working { capture_id } => {
            format!("working capture {}", capture_id.short())
        }
        ReferenceVersion::Candidate { candidate_id } => basis_label(
            &Basis::Candidate {
                candidate_id: candidate_id.clone(),
            },
            detail,
        ),
    }
}

pub fn reference_label(reference: &Reference, detail: Option<&ObjectiveDetail>) -> String {
    let mut parts = vec![
        reference.path.display.clone(),
        reference_version_label(&reference.version, detail),
        side_label(reference.side).to_owned(),
    ];
    if let Some(line) = reference.line_hint {
        parts.push(format!("line {line}"));
    }
    parts.push(format!(
        "bytes {}–{}",
        reference.byte_start, reference.byte_end
    ));
    parts.join(" · ")
}

pub fn input_line(input: &InputSummary) -> String {
    format!(
        "{} · {}",
        mode_label(input.mode),
        delivery_label(input.delivery, input.blocked_reason.as_deref())
    )
}

pub fn command_line(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            if arg.is_empty()
                || arg
                    .chars()
                    .any(|c| c.is_whitespace() || "'\"\\$`".contains(c))
            {
                format!("'{}'", arg.replace('\'', "'\\''"))
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn verification_summary_line(v: &VerificationSummary) -> String {
    let (sign, label) = verification_result(v.state);
    match v.exit_code {
        Some(code) => format!("{sign} {label} · exit {code}"),
        None => format!("{sign} {label}"),
    }
}

pub fn split_argv(input: &str) -> Result<Vec<String>, &'static str> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_arg = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => current.push(ch),
                        None => return Err("Unclosed single quote"),
                    }
                }
            }
            '"' => {
                in_arg = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(ch @ ('"' | '\\')) => current.push(ch),
                            Some(ch) => {
                                current.push('\\');
                                current.push(ch);
                            }
                            None => return Err("Unclosed double quote"),
                        },
                        Some(ch) => current.push(ch),
                        None => return Err("Unclosed double quote"),
                    }
                }
            }
            '\\' => {
                in_arg = true;
                current.push(chars.next().ok_or("Trailing backslash")?);
            }
            c if c.is_whitespace() => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            c => {
                in_arg = true;
                current.push(c);
            }
        }
    }
    if in_arg {
        args.push(current);
    }
    if args.is_empty() {
        return Err("Enter a command to run");
    }
    Ok(args)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Done,
    Current,
    Upcoming,
}

pub const JOURNEY_STEPS: [&str; 5] = [
    "Inspect",
    "Feedback",
    "Agent revision",
    "Review",
    "Integrate",
];

pub fn journey(detail: &ObjectiveDetail, has_inputs: bool) -> [StepState; 5] {
    use StepState::{Current, Done, Upcoming};
    let summary = &detail.summary;
    let current = match summary.lifecycle {
        _ if summary.attention == Attention::Archived && !summary.lifecycle.is_archived() => None,
        Lifecycle::Defined | Lifecycle::Discarded | Lifecycle::CompletedNoChanges => None,
        Lifecycle::Integrated => Some(5),
        Lifecycle::Approved => Some(4),
        Lifecycle::Frozen => Some(3),
        Lifecycle::Active => {
            let running = matches!(
                summary.execution.state,
                ExecutionState::Running | ExecutionState::Starting
            );
            if running && has_inputs {
                Some(2)
            } else if detail.unsent_drafts > 0 {
                Some(1)
            } else {
                Some(0)
            }
        }
    };
    let mut steps = [Upcoming; 5];
    if let Some(current) = current {
        for (index, step) in steps.iter_mut().enumerate() {
            *step = match index.cmp(&current) {
                std::cmp::Ordering::Less => Done,
                std::cmp::Ordering::Equal => Current,
                std::cmp::Ordering::Greater => Upcoming,
            };
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_sizes_use_binary_units() {
        assert_eq!(byte_size(812), "812 bytes");
        assert_eq!(byte_size(4096), "4.0 KiB");
        assert_eq!(byte_size((65 << 20) + (1 << 19)), "65.5 MiB");
        assert_eq!(byte_size(1 << 30), "1.0 GiB");
    }

    #[test]
    fn argv_splitting_without_a_shell() {
        assert_eq!(
            split_argv("git diff --check").unwrap(),
            ["git", "diff", "--check"]
        );
        assert_eq!(
            split_argv(r#"npm test -- --grep "retry \"timeout\"" 'a b' c\ d"#).unwrap(),
            [
                "npm",
                "test",
                "--",
                "--grep",
                "retry \"timeout\"",
                "a b",
                "c d"
            ]
        );
        assert_eq!(split_argv("echo ''").unwrap(), ["echo", ""]);
        assert!(split_argv("  ").is_err());
        assert!(split_argv("echo 'x").is_err());
    }

    #[test]
    fn relative_time_labels() {
        let now = Timestamp(10_000_000);
        assert_eq!(relative_time(now, Timestamp(now.0 - 5_000)), "just now");
        assert_eq!(
            relative_time(now, Timestamp(now.0 - 12 * 60_000)),
            "12 min ago"
        );
        assert_eq!(
            relative_time(now, Timestamp(now.0 - 2 * 3_600_000)),
            "2 hours ago"
        );
    }

    #[test]
    fn version_line_adds_only_what_the_selector_does_not_say() {
        use crate::presentation::state::model::{CandidateId, GitId, TargetRef};
        let target = TargetRef {
            branch: "main".into(),
            commit: GitId::sha1("86fc063aaaabbbbccccddddeeeeffff000011112"),
        };
        let (short, full) =
            version_line(&Basis::Working, "working capture bfe258b0", None, &target);
        assert_eq!(short, "Captured · may change · vs main@86fc063");
        assert!(full.contains("working capture bfe258b0"));
        assert!(full.contains(&target.commit.hex));
        let candidate = Basis::Candidate {
            candidate_id: CandidateId::new("c1"),
        };
        for status in [
            CandidateStatus::Current,
            CandidateStatus::Historical,
            CandidateStatus::Invalidated,
        ] {
            let (short, full) =
                version_line(&candidate, "Candidate 2 — frozen", Some(status), &target);
            assert_eq!(short, "Frozen · vs main@86fc063");
            assert!(!short.contains("Candidate"));
            assert!(full.contains("Candidate 2 — frozen"));
            assert!(full.contains(candidate_status(status)));
        }
    }

    #[test]
    fn sentences_are_capitalized_and_terminated() {
        assert_eq!(sentence(" not connected "), "Not connected.");
        assert_eq!(sentence("Done."), "Done.");
        assert_eq!(sentence("why?"), "Why?");
        assert_eq!(sentence(""), "");
        assert_eq!(
            delivery_label(DeliveryState::Blocked, Some("the agent isn't running")),
            "Accepted · blocked. The agent isn't running."
        );
    }

    #[test]
    fn command_line_quotes_only_when_needed() {
        let argv = vec!["npm".to_owned(), "run".to_owned(), "a b".to_owned()];
        assert_eq!(command_line(&argv), "npm run 'a b'");
    }
}
