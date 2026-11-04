//! Store updates for service events.

use lade_protocol::client::Event;

use crate::presentation::state::agent_session::AgentSessionUpdate;
use crate::presentation::state::model::{ActivityItemId, ObjectiveId};
use crate::presentation::state::store::StoreUpdate;
use crate::transport::{agent_session, convert};

pub fn event_updates(event: Event) -> Vec<StoreUpdate> {
    vec![match event {
        Event::ObjectiveChanged(s) => StoreUpdate::ObjectiveChanged(convert::summary(&s)),
        Event::ObjectiveDetail(d) => StoreUpdate::ObjectiveDetail(Box::new(convert::detail(&d))),
        Event::ActivityAppended(a) => StoreUpdate::ActivityAppended {
            objective_id: ObjectiveId::new(a.objective_id.as_str()),
            items: a.items.iter().map(convert::activity_item).collect(),
        },
        Event::ActivityDelta(d) => StoreUpdate::ActivityDelta {
            objective_id: ObjectiveId::new(d.objective_id.as_str()),
            item_id: ActivityItemId::new(d.item_id.as_str()),
            append_text: d.append_text,
        },
        Event::DraftChanged(d) => StoreUpdate::DraftChanged(convert::draft(&d)),
        Event::InputChanged(i) => {
            let input = convert::input(&i);
            StoreUpdate::InputChanged {
                objective_id: input.objective_id.clone(),
                input,
            }
        }
        Event::WorktreeChanged(w) => StoreUpdate::WorktreeChanged {
            objective_id: ObjectiveId::new(w.objective_id.as_str()),
        },
        Event::VerificationChanged(v) => {
            StoreUpdate::VerificationChanged(convert::verification(&v))
        }
        Event::ServiceNotice(n) => StoreUpdate::Notice(convert::notice(&n)),
        Event::SessionChanged(s) => {
            let (objective_id, session) = agent_session::session(&s);
            StoreUpdate::AgentSession(AgentSessionUpdate::Session {
                objective_id,
                session,
            })
        }
        Event::UiRequest(r) => {
            StoreUpdate::AgentSession(AgentSessionUpdate::DialogOpened(agent_session::dialog(&r)))
        }
        Event::UiClosed(c) => StoreUpdate::AgentSession(AgentSessionUpdate::DialogClosed {
            ui_id: c.ui_id.as_str().to_owned(),
        }),
        Event::ResourcesChanged(i) => {
            StoreUpdate::AgentSession(AgentSessionUpdate::Inventory(agent_session::inventory(&i)))
        }
        Event::TerminalOutput(o) => StoreUpdate::TerminalOutput {
            terminal_id: o.terminal_id.as_str().to_owned(),
            seq: o.seq.value(),
            data: o.data,
            dropped: o.dropped_bytes.value(),
        },
        Event::TerminalClosed(c) => StoreUpdate::TerminalClosed {
            terminal_id: c.terminal_id.as_str().to_owned(),
            end: convert::terminal_end(&c),
        },
    }]
}

#[cfg(test)]
mod tests {
    use lade_protocol::client::{
        self as wire, ActivityDelta, TerminalCloseReason, TerminalClosed, TerminalOutput, UiClosed,
        WorktreeChanged,
    };
    use lade_protocol::types::{Counter, Id, UiCloseReason};

    use super::*;
    use crate::presentation::terminal::TerminalEnd;
    use crate::transport::requests::tests::{O, event};

    fn id(byte: u8) -> Id {
        Id::from_random_bytes([byte; 16])
    }

    fn only(event: Event) -> StoreUpdate {
        let mut updates = event_updates(event);
        assert_eq!(updates.len(), 1, "each event is one store update");
        updates.remove(0)
    }

    #[test]
    fn streamed_text_keeps_its_objective_and_item() {
        let update = only(Event::ActivityDelta(ActivityDelta {
            objective_id: Id::parse(O).unwrap(),
            item_id: id(7),
            append_text: "partial".into(),
        }));
        assert_eq!(
            update,
            StoreUpdate::ActivityDelta {
                objective_id: ObjectiveId::new(O),
                item_id: ActivityItemId::new(id(7).as_str()),
                append_text: "partial".into(),
            }
        );
    }

    #[test]
    fn a_worktree_change_only_flags_its_objective() {
        let update = only(Event::WorktreeChanged(WorktreeChanged {
            objective_id: Id::parse(O).unwrap(),
            capture_id: id(3),
        }));
        assert_eq!(
            update,
            StoreUpdate::WorktreeChanged {
                objective_id: ObjectiveId::new(O),
            }
        );
    }

    #[test]
    fn terminal_output_carries_its_sequence_and_dropped_bytes() {
        let update = only(Event::TerminalOutput(TerminalOutput {
            objective_id: Id::parse(O).unwrap(),
            terminal_id: id(4),
            seq: Counter::new(12),
            data: "$ ".into(),
            dropped_bytes: Counter::new(512),
        }));
        assert_eq!(
            update,
            StoreUpdate::TerminalOutput {
                terminal_id: id(4).as_str().to_owned(),
                seq: 12,
                data: "$ ".into(),
                dropped: 512,
            }
        );
    }

    #[test]
    fn an_exited_terminal_reports_its_code_and_signal() {
        let update = only(Event::TerminalClosed(TerminalClosed {
            objective_id: Id::parse(O).unwrap(),
            terminal_id: id(4),
            reason: TerminalCloseReason::Exited,
            exit_code: Some(Counter::new(2)),
            signal: None,
        }));
        assert_eq!(
            update,
            StoreUpdate::TerminalClosed {
                terminal_id: id(4).as_str().to_owned(),
                end: TerminalEnd::Exited {
                    code: Some(2),
                    signal: None,
                },
            }
        );
    }

    #[test]
    fn a_closed_extension_dialog_is_named_by_its_ui_id() {
        let update = only(Event::UiClosed(UiClosed {
            objective_id: Id::parse(O).unwrap(),
            ui_id: id(9),
            reason: UiCloseReason::Answered,
        }));
        assert_eq!(
            update,
            StoreUpdate::AgentSession(AgentSessionUpdate::DialogClosed {
                ui_id: id(9).as_str().to_owned(),
            })
        );
    }

    #[test]
    fn an_objective_change_becomes_its_summary() {
        let wire::Event::ObjectiveChanged(summary) = event(1).event else {
            unreachable!("the shared fixture event is an objective change")
        };
        let StoreUpdate::ObjectiveChanged(converted) =
            only(Event::ObjectiveChanged(summary.clone()))
        else {
            panic!("an objective change");
        };
        assert_eq!(
            converted.objective_id.as_str(),
            summary.objective_id.as_str()
        );
        assert_eq!(converted.revision, summary.revision.value());
    }
}
