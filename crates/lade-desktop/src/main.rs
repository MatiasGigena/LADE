#[allow(
    dead_code,
    reason = "presentation API surface shared by preview, tests and the live transport"
)]
mod presentation;
mod transport;
#[allow(
    dead_code,
    reason = "view helpers kept for compositions not used by every launch mode"
)]
mod views;

use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::{Application, gio, glib, prelude::*};

use presentation::intents::IntentSink;
use presentation::model::{ConnectionState, Timestamp};
use presentation::preview::{PreviewResponder, PreviewVariant, preview_state};
use presentation::store::{PresentationState, Store};
use transport::Transport;
use transport::connection::{self, Config, SpawnService};

const USAGE: &str = "usage: lade-desktop [--runtime-dir DIR] [--state-dir DIR] [--no-spawn] [--open PATH]\n       lade-desktop --preview | --preview=disconnected\n       lade-desktop --version";

#[derive(Debug, Default, PartialEq, Eq)]
struct Live {
    runtime_dir: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    no_spawn: bool,
    open: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Live(Live),
    Preview(PreviewVariant),
}

fn absolute(path: OsString) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(path))
}

fn parse_args(args: &[OsString]) -> Result<Mode, String> {
    match args {
        [flag] if flag == "--preview" => return Ok(Mode::Preview(PreviewVariant::Standard)),
        [flag] if flag == "--preview=disconnected" => {
            return Ok(Mode::Preview(PreviewVariant::Disconnected));
        }
        _ => {}
    }
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
            _ => return Err(USAGE.into()),
        };
        let value = args.next().filter(|v| !v.is_empty()).ok_or(USAGE)?;
        *slot = Some(absolute(value));
    }
    Ok(Mode::Live(live))
}

fn live_config(live: &Live) -> Result<Config, String> {
    let runtime_dir = connection::runtime_dir(
        live.runtime_dir.as_deref(),
        live.state_dir.as_deref(),
        |name| std::env::var(name).ok(),
    )
    .ok_or("no runtime directory: set XDG_RUNTIME_DIR or HOME, or pass --runtime-dir")?;
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
    let mode = match parse_args(&args) {
        Ok(mode) => mode,
        Err(usage) => {
            eprintln!("{usage}");
            return glib::ExitCode::from(2);
        }
    };
    let config = match &mode {
        Mode::Live(live) => match live_config(live) {
            Ok(config) => Some(config),
            Err(message) => {
                eprintln!("lade-desktop: {message}");
                return glib::ExitCode::from(2);
            }
        },
        Mode::Preview(_) => None,
    };
    // Live windows are single-instance: a second launch activates the running window (GApplication
    // on the session bus) instead of opening another approval authority; the service's single
    // controlling lease refuses any other client anyway. Without a session bus GApplication runs
    // non-unique and the lease is the only guard. Preview windows are fixture data and may coexist.
    let live = matches!(mode, Mode::Live(_));
    let app = Application::builder()
        .application_id(if live {
            "dev.lade.Desktop"
        } else {
            "dev.lade.Desktop.Preview"
        })
        .flags(if live {
            gio::ApplicationFlags::FLAGS_NONE
        } else {
            gio::ApplicationFlags::NON_UNIQUE
        })
        // Logout: the session manager asks before ending the session (`query-end`).
        .register_session(live)
        .build();
    app.connect_startup(|_| views::load_css());
    let mode = Rc::new(mode);
    // The views are owned here for the lifetime of the application.
    let windows: Rc<std::cell::RefCell<Vec<Rc<views::App>>>> = Rc::default();
    if live {
        // Notification clicks navigate in the existing window; they never approve or send.
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
        // Logout stops the service according to policy: no lingering work, nothing resumes.
        let ending = windows.clone();
        app.connect_query_end(move |_| {
            for window in ending.borrow().iter() {
                window.logout();
            }
        });
    }
    app.connect_activate(move |app| {
        // A second activation (another launch, or a notification) focuses the existing window.
        if let Some(existing) = windows.borrow().first() {
            existing.present();
            return;
        }
        let window = match (&*mode, &config) {
            (Mode::Preview(variant), _) => {
                let now = Timestamp(glib::real_time() / 1000);
                let monotonic = u64::try_from(glib::monotonic_time() / 1000).unwrap_or(0);
                let store = Store::new(preview_state(now, monotonic, *variant));
                let responder: Rc<dyn IntentSink> =
                    Rc::new(PreviewResponder::new(Rc::downgrade(&store), now));
                let sink = Rc::new(views::bridge::DeferredSink::new(responder));
                views::App::new(app, store, sink)
            }
            (Mode::Live(_), Some(config)) => {
                let store = Store::new(PresentationState::new(ConnectionState::Connecting));
                let transport = Rc::new(Transport::new());
                let window = views::App::new(app, store, transport.clone());
                window.set_notifier(Rc::new(views::notify::DesktopNotifier::new(app)));
                if transport
                    .start(config.clone(), window.update_sender())
                    .is_err()
                {
                    eprintln!("lade-desktop: could not start the transport thread");
                }
                window
            }
            (Mode::Live(_), None) => return,
        };
        window.load_initial();
        window.present();
        windows.borrow_mut().push(window);
    });
    // GTK must not interpret LADE's own flags.
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
        assert_eq!(parse_args(&[]), Ok(Mode::Live(Live::default())));
        assert_eq!(
            parse_args(&args(&["--preview"])),
            Ok(Mode::Preview(PreviewVariant::Standard))
        );
    }

    #[test]
    fn live_flags_parse_once_each() {
        let Ok(Mode::Live(live)) = parse_args(&args(&[
            "--runtime-dir",
            "/tmp/r",
            "--no-spawn",
            "--open",
            "/tmp/repo",
            "--state-dir",
            "/tmp/s",
        ])) else {
            panic!("live mode");
        };
        assert_eq!(live.runtime_dir, Some(PathBuf::from("/tmp/r")));
        assert_eq!(live.state_dir, Some(PathBuf::from("/tmp/s")));
        assert_eq!(live.open, Some(PathBuf::from("/tmp/repo")));
        assert!(live.no_spawn);
        assert!(parse_args(&args(&["--no-spawn", "--no-spawn"])).is_err());
        assert!(parse_args(&args(&["--runtime-dir"])).is_err());
        assert!(parse_args(&args(&["--preview", "--no-spawn"])).is_err());
        assert!(parse_args(&args(&["--bogus"])).is_err());
    }
}
