use gtk4::{Application, ApplicationWindow, Label, gio, prelude::*};

fn main() -> gtk4::glib::ExitCode {
    if std::env::args_os().skip(1).eq(["--version"]) {
        println!(
            "LADE {} protocol {}",
            lade_protocol::BUNDLE,
            lade_protocol::VERSION
        );
        return gtk4::glib::ExitCode::SUCCESS;
    }
    let app = Application::builder()
        .application_id("dev.lade.BuildShell")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(|app| {
        // A native build checkpoint, not the workbench or simulated product authority.
        let status = Label::new(Some("Build shell — service unavailable (IMP-003)"));
        status.set_wrap(true);
        status.set_margin_top(24);
        status.set_margin_bottom(24);
        status.set_margin_start(24);
        status.set_margin_end(24);
        let window = ApplicationWindow::builder()
            .application(app)
            .title("LADE")
            .default_width(640)
            .default_height(360)
            .child(&status)
            .build();
        window.present();
    });
    app.run()
}
