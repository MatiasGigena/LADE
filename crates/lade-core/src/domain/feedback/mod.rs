//! Feedback to the agent: Drafts anchored to inspected content, their acceptance (alone or as a
//! batch with a note, or as a plain message) into an Accepted Input, and its delivery state.

mod delivery;
mod draft;
mod input;
#[cfg(test)]
mod tests;

pub use delivery::{BlockedReason, DeliveryState, DispatchDecision};
pub use draft::{Draft, DraftRecord, DraftState, SaveDraft, save_draft};
pub use input::{
    Acceptance, AcceptedInput, BatchEntry, FeedbackItem, InputMode, MAX_BATCH_ITEMS,
    accept_feedback, accept_message, input_refusal, render_feedback,
};
