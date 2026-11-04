#[allow(
    dead_code,
    reason = "presentation API surface shared by tests and the live transport"
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

use presentation::state::model::ConnectionState;
use presentation::state::store::{PresentationState, Store};
use transport::Transport;
use transport::connection::{self, Config, SpawnService};

const USAGE: &str = "usage: lade-desktop [--runtime-dir DIR] [--state-dir DIR] [--no-spawn] [--open PATH]\n       lade-desktop --version";

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

fn parse_args(args: &[OsString]) -> Result<Live, String> {
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
    Ok(live)
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
    let config = match parse_args(&args) {
        Ok(live) => match live_config(&live) {
            Ok(config) => config,
            Err(message) => {
                eprintln!("lade-desktop: {message}");
                return glib::ExitCode::from(2);
            }
        },
        Err(usage) => {
            eprintln!("{usage}");
            return glib::ExitCode::from(2);
        }
    };
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
        let transport = Rc::new(Transport::new());
        let window = views::App::new(app, store, transport.clone());
        window.set_notifier(Rc::new(views::notify::DesktopNotifier::new(app)));
        if transport
            .start(config.clone(), window.update_sender())
            .is_err()
        {
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
        assert!(parse_args(&args(&["--preview"])).is_err());
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
        assert!(parse_args(&args(&["--no-spawn", "--no-spawn"])).is_err());
        assert!(parse_args(&args(&["--runtime-dir"])).is_err());
        assert!(parse_args(&args(&["--bogus"])).is_err());
    }
}
