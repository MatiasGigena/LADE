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
