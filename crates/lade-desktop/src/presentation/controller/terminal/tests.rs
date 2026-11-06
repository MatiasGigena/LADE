use super::*;
use crate::presentation::controller::tests::{main_selected, setup};
use crate::presentation::state::model::ConnectionState;
use crate::presentation::state::store::StoreUpdate;
use crate::presentation::terminal::{TerminalEnd, TerminalMode};

const TERMINAL: &str = "00000000-0000-4000-8000-000000000042";

fn opened(controller: &Controller, id: &ObjectiveId) {
    controller.store.apply(StoreUpdate::TerminalOpened {
        objective_id: id.clone(),
        terminal_id: TERMINAL.into(),
        mode: TerminalMode::ReadWrite,
        cols: 80,
        rows: 24,
    });
}

#[test]
fn terminal_lifecycle_in_the_controller() {
    let (controller, sink, id) = main_selected();
    assert_eq!(
        controller.terminal_input("ls\r"),
        Err("No terminal is open".to_owned()),
        "no terminal yet"
    );
    controller.open_terminal(80, 24).unwrap();
    controller.open_terminal(80, 24).unwrap();
    assert_eq!(
        sink.intents
            .borrow()
            .iter()
            .filter(|i| matches!(i, Intent::OpenTerminal { .. }))
            .count(),
        1,
        "one open in flight"
    );
    controller.store.apply(StoreUpdate::TerminalOpened {
        objective_id: id.clone(),
        terminal_id: "00000000-0000-4000-8000-000000000042".into(),
        mode: TerminalMode::ReadOnly,
        cols: 80,
        rows: 24,
    });
    assert_eq!(controller.terminal_input(&"x".repeat(5000)), Ok(()));
    let inputs = sink
        .intents
        .borrow()
        .iter()
        .filter(|i| matches!(i, Intent::TerminalInput { .. }))
        .count();
    assert_eq!(inputs, 2, "long input is chunked");
    controller.resize_terminal(100, 30);
    controller.resize_terminal(100, 30);
    assert_eq!(
        sink.intents
            .borrow()
            .iter()
            .filter(|i| matches!(
                i,
                Intent::ResizeTerminal {
                    cols: 100,
                    rows: 30,
                    ..
                }
            ))
            .count(),
        1
    );
    controller.store.apply(StoreUpdate::TerminalOutput {
        terminal_id: "00000000-0000-4000-8000-000000000042".into(),
        seq: 1,
        data: "hello".into(),
        dropped: 0,
    });
    controller.store.apply(StoreUpdate::TerminalClosed {
        terminal_id: "00000000-0000-4000-8000-000000000042".into(),
        end: TerminalEnd::ReviewFreeze,
    });
    assert_eq!(
        controller.terminal_input("x"),
        Err("No terminal is open".to_owned()),
        "an ended terminal takes no input"
    );
    controller.open_terminal(80, 24).unwrap();
    controller.store.apply(StoreUpdate::TerminalOpened {
        objective_id: id.clone(),
        terminal_id: "00000000-0000-4000-8000-000000000043".into(),
        mode: TerminalMode::ReadWrite,
        cols: 80,
        rows: 24,
    });
    controller
        .store
        .apply(StoreUpdate::Connection(ConnectionState::Reconnecting {
            attempt: 1,
        }));
    assert_eq!(
        controller
            .store
            .state()
            .terminal_for(&id)
            .map(|t| t.status.clone()),
        Some(TerminalStatus::Ended(TerminalEnd::ConnectionLost))
    );
}

#[test]
fn a_terminal_needs_a_selected_objective() {
    let (controller, sink) = setup();
    assert_eq!(
        controller.open_terminal(80, 24),
        Err("Select an Objective first".into())
    );
    controller.resize_terminal(100, 30);
    controller.close_terminal();
    assert_eq!(
        controller.terminal_input("x"),
        Err("Select an Objective first".into())
    );
    assert!(sink.intents.borrow().is_empty());
}

#[test]
fn resizing_an_opening_terminal_resizes_its_screen_without_telling_the_service() {
    let (controller, sink, id) = main_selected();
    controller.open_terminal(80, 24).unwrap();
    sink.intents.borrow_mut().clear();
    controller.resize_terminal(120, 40);
    assert!(sink.intents.borrow().is_empty(), "no terminal id yet");
    assert_eq!(
        controller
            .store
            .state()
            .terminal_for(&id)
            .unwrap()
            .screen
            .size(),
        (120, 40)
    );
}

#[test]
fn closing_is_sent_only_for_an_open_terminal() {
    let (controller, sink, id) = main_selected();
    controller.close_terminal();
    controller.open_terminal(80, 24).unwrap();
    sink.intents.borrow_mut().clear();
    controller.close_terminal();
    assert!(sink.intents.borrow().is_empty(), "still opening");
    opened(&controller, &id);
    controller.close_terminal();
    assert_eq!(
        sink.intents.borrow().as_slice(),
        [Intent::CloseTerminal {
            objective_id: id,
            terminal_id: TERMINAL.into(),
        }]
    );
}

#[test]
fn input_is_sent_in_order_to_the_open_terminal() {
    let (controller, sink, id) = main_selected();
    controller.open_terminal(80, 24).unwrap();
    opened(&controller, &id);
    sink.intents.borrow_mut().clear();
    assert_eq!(controller.terminal_input("ls\r"), Ok(()));
    assert_eq!(
        sink.intents.borrow().as_slice(),
        [Intent::TerminalInput {
            objective_id: id,
            terminal_id: TERMINAL.into(),
            data: "ls\r".into(),
        }]
    );
}
