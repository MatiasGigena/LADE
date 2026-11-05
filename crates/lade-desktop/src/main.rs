//! The LADE desktop client: a GTK 4 application that connects to the local LADE service over its
//! versioned protocol and presents workspaces, objectives, review evidence and Agent Sessions.
//!
//! The crate is layered: `transport` owns the service connection on a background thread and
//! converts wire messages into presentation updates; `presentation` holds the GTK-free state
//! store, controller and text formatting; `views` renders that state with GTK widgets.

mod presentation;
mod transport;
mod views;

use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::{Application, gio, glib, prelude::*};

use presentation::state::model::ConnectionState;
use presentation::state::store::{PresentationState, Store};
use transport::Transport;
use transport::connection::{self, Config, SpawnService};

const USAGE: &str = "usage: lade-desktop [--runtime-dir DIR] [--state-dir DIR] [--no-spawn] [--open PATH]\n       lade-desktop --version";

/// Why the command line could not be turned into a launch configuration.
#[derive(Debug, PartialEq, Eq)]
enum LaunchError {
    /// The arguments did not match [`USAGE`].
    Usage,
    /// Neither the flags nor the environment name a runtime directory.
    NoRuntimeDir,
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage => f.write_str(USAGE),
            Self::NoRuntimeDir => f.write_str(
                "lade-desktop: no runtime directory: set XDG_RUNTIME_DIR or HOME, or pass --runtime-dir",
            ),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Live {
    runtime_dir: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    no_spawn: bool,
    open: Option<PathBuf>,
}

fn absolute(path: OsString) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(path))
}

fn parse_args(args: &[OsString]) -> Result<Live, LaunchError> {
    let mut live = Live::default();
    let mut args = args.iter().cloned();
    while let Some(flag) = args.next() {
        let slot = match flag.to_str() {
            Some("--no-spawn") if !live.no_spawn => {
                live.no_spawn = true;
                continue;
            }
            Some("--runtime-dir") if live.runtime_dir.is_none() => &mut live.runtime_dir,
            Some("--state-dir") if live.state_dir.is_none() => &mut live.state_dir,
            Some("--open") if live.open.is_none() => &mut live.open,
            _ => return Err(LaunchError::Usage),
        };
        let value = args
            .next()
            .filter(|v| !v.is_empty())
            .ok_or(LaunchError::Usage)?;
        *slot = Some(absolute(value));
    }
    Ok(live)
}

fn live_config(live: &Live) -> Result<Config, LaunchError> {
    let runtime_dir = connection::runtime_dir(
        live.runtime_dir.as_deref(),
        live.state_dir.as_deref(),
        |name| std::env::var(name).ok(),
    )
    .ok_or(LaunchError::NoRuntimeDir)?;
    let spawn = (!live.no_spawn)
        .then(connection::sibling_service)
        .flatten()
        .map(|program| SpawnService {
            program,
            state_dir: live.state_dir.clone(),
        });
    Ok(Config {
        runtime_dir,
        spawn,
        open: live.open.clone(),
    })
}

fn main() -> glib::ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--version" {
        println!(
            "LADE {} protocol {}",
            lade_protocol::BUNDLE,
            lade_protocol::VERSION
        );
        return glib::ExitCode::SUCCESS;
    }
    let config = match parse_args(&args).and_then(|live| live_config(&live)) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return glib::ExitCode::from(2);
        }
    };
    let env = views::Environment::from_process();
    gtk4::disable_setlocale();
    let app = Application::builder()
        .application_id("dev.lade.Desktop")
        .flags(gio::ApplicationFlags::FLAGS_NONE)
        .register_session(true)
        .build();
    app.connect_startup(|_| views::load_css());
    let windows: Rc<std::cell::RefCell<Vec<Rc<views::App>>>> = Rc::default();
    let action = gio::SimpleAction::new(
        views::notify::OPEN_OBJECTIVE_ACTION,
        Some(glib::VariantTy::STRING),
    );
    let open = windows.clone();
    action.connect_activate(move |_, target| {
        let target = target.and_then(glib::Variant::str);
        if let (Some(window), Some(target)) = (open.borrow().first(), target) {
            window.open_from_notification(target);
        }
    });
    app.add_action(&action);
    let ending = windows.clone();
    app.connect_query_end(move |_| {
        for window in ending.borrow().iter() {
            window.logout();
        }
    });
    app.connect_activate(move |app| {
        if let Some(existing) = windows.borrow().first() {
            existing.present();
            return;
        }
        let store = Store::new(PresentationState::new(ConnectionState::Connecting));
        let updates = transport::bridge::connect(&store, 1024);
        let transport = Rc::new(Transport::new());
        let window = views::App::new(app, store, transport.clone(), env.clone());
        window.set_notifier(Rc::new(views::notify::DesktopNotifier::new(app)));
        if transport.start(config.clone(), updates).is_err() {
            eprintln!("lade-desktop: could not start the transport thread");
        }
        window.load_initial();
        window.present();
        windows.borrow_mut().push(window);
    });
    app.run_with_args::<&str>(&[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn default_launch_is_live() {
        assert_eq!(parse_args(&[]), Ok(Live::default()));
        assert_eq!(parse_args(&args(&["--preview"])), Err(LaunchError::Usage));
    }

    #[test]
    fn live_flags_parse_once_each() {
        let Ok(live) = parse_args(&args(&[
            "--runtime-dir",
            "/tmp/r",
            "--no-spawn",
            "--open",
            "/tmp/repo",
            "--state-dir",
            "/tmp/s",
        ])) else {
            panic!("live flags");
        };
        assert_eq!(live.runtime_dir, Some(PathBuf::from("/tmp/r")));
        assert_eq!(live.state_dir, Some(PathBuf::from("/tmp/s")));
        assert_eq!(live.open, Some(PathBuf::from("/tmp/repo")));
        assert!(live.no_spawn);
        for bad in [
            &["--no-spawn", "--no-spawn"][..],
            &["--runtime-dir"],
            &["--bogus"],
        ] {
            assert_eq!(parse_args(&args(bad)), Err(LaunchError::Usage));
        }
    }
}
