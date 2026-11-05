//! How a rejected request is explained: refusal text and the store updates per [`Context`].

use lade_protocol::client::{self as wire, Rejection};

use super::{Context, Effects, refresh_objective};
use crate::presentation::controller::intents::Intent;
use crate::presentation::state::model::{
    ActionSurface, DraftId, NoticeLevel, NoticeScope, Refusal, ServiceNotice,
};
use crate::presentation::state::store::StoreUpdate;
use crate::presentation::text::format::sentence;
use crate::transport::convert;

fn plain_reason(rejection: &Rejection) -> String {
    match rejection.detail.trim() {
        "" => convert::fallback_reason(rejection.code).to_owned(),
        detail => sentence(detail),
    }
}

fn reason_and_advice(rejection: &Rejection) -> String {
    let mut text = plain_reason(rejection);
    if let Some(advice) = convert::advice(rejection.code, rejection.action)
        && !text.contains(advice)
    {
        text.push(' ');
        text.push_str(advice);
    }
    text
}

fn technical_details(rejection: &Rejection) -> String {
    let code = convert::error_code(rejection.code);
    if rejection.detail.is_empty() {
        code.to_owned()
    } else {
        format!("{code}: {}", rejection.detail)
    }
}

pub fn refusal(action: &str, rejection: &Rejection) -> Refusal {
    Refusal {
        message: format!("Couldn't {action}. {}", reason_and_advice(rejection)),
        details: technical_details(rejection),
    }
}

pub fn rejection_message(action: &str, rejection: &Rejection) -> String {
    refusal(action, rejection).message
}

/// Names the submitted drafts the service reported as stale, so the reviewer knows which to fix.
fn stale_drafts_note(batch: &[DraftId], rejection: &Rejection) -> Option<String> {
    let stale: Vec<&str> = batch
        .iter()
        .filter(|d| {
            rejection
                .stale_drafts
                .iter()
                .any(|s| s.as_str() == d.as_str())
        })
        .map(DraftId::short)
        .collect();
    let (plural, pronoun) = match stale.len() {
        0 => return None,
        1 => ("", "it"),
        _ => ("s", "them"),
    };
    Some(format!(
        " Stale draft{plural}: {}. Re-anchor or remove {pronoun} and submit again.",
        stale.join(", ")
    ))
}

pub fn rejection_effects(context: &Context, rejection: &Rejection) -> Effects {
    let mut effects = Effects::default();
    let code = convert::error_code(rejection.code);
    let mut refused = refusal(&context.action(), rejection);
    let updates = &mut effects.updates;
    let notice = |scope, refused: Refusal| {
        StoreUpdate::Notice(ServiceNotice {
            level: NoticeLevel::Warning,
            code: code.to_owned(),
            message: refused.message,
            scope,
        })
    };
    match context {
        Context::Objective(o) => updates.push(StoreUpdate::ObjectiveLoadFailed {
            objective_id: o.clone(),
            message: refused.message,
        }),
        Context::Changes(o, basis) => updates.push(StoreUpdate::ChangesFailed {
            objective_id: o.clone(),
            basis: basis.clone(),
            message: refused.message,
        }),
        Context::Diff(o, _, path) | Context::File(o, _, path, _) => {
            updates.push(StoreUpdate::ViewerFailed {
                objective_id: o.clone(),
                path: path.clone(),
                message: refused.message,
            });
        }
        Context::OpenWorkspace => {
            if rejection.code == wire::ErrorCode::InvalidRequest {
                refused.message.push_str(" Choose the repository folder containing .git, not .git itself. It needs at least one commit.");
            }
            // Scoped so a later successful open resolves only this notice.
            updates.push(notice(NoticeScope::WorkspaceOpen, refused));
        }
        Context::Search { .. } => updates.push(StoreUpdate::SearchFailed {
            message: refused.message,
        }),
        Context::Terminal(o, _) => updates.push(StoreUpdate::TerminalRefused {
            objective_id: o.clone(),
            refusal: refused,
        }),
        Context::Stop
        | Context::Snapshot
        | Context::Activity(_)
        | Context::Drafts(_)
        | Context::Inputs(_)
        | Context::Models(_)
        | Context::Tree(_)
        | Context::Resources => updates.push(notice(NoticeScope::General, refused)),
        Context::DraftDelete(_, draft_id) => updates.push(StoreUpdate::DraftDeleteRefused {
            draft_id: draft_id.clone(),
            refusal: refused,
        }),
        Context::Command {
            objective_id,
            saved_draft,
            batch,
            surface,
            ..
        } => {
            if let Some(note) = stale_drafts_note(batch, rejection) {
                refused.message.push_str(&note);
            }
            match (saved_draft, objective_id) {
                (Some(draft_id), _) => updates.push(StoreUpdate::DraftSaveRefused {
                    draft_id: draft_id.clone(),
                    refusal: Refusal {
                        message: reason_and_advice(rejection),
                        details: refused.details,
                    },
                }),
                (None, Some(o)) => {
                    updates.push(StoreUpdate::CommandStatus {
                        objective_id: o.clone(),
                        status: None,
                    });
                    updates.push(StoreUpdate::Refused {
                        objective_id: o.clone(),
                        surface: surface.unwrap_or(ActionSurface::Review),
                        refusal: refused,
                    });
                }
                (None, None) => updates.push(notice(NoticeScope::General, refused)),
            }
            if let Some(o) = objective_id {
                effects.follow_ups.push(refresh_objective(o));
                if !batch.is_empty() || saved_draft.is_some() {
                    effects.follow_ups.push(Intent::LoadDrafts {
                        objective_id: o.clone(),
                    });
                }
            }
        }
    }
    effects
}

#[cfg(test)]
mod tests;
