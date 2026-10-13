//! Read-only diff / full-file viewer (GtkSourceView). Old/new line gutters and a sign column are
//! custom gutter renderers; added/deleted lines get semantic paragraph fills. Selections map to
//! exact source bytes through `presentation::text_map`. Syntax is highlighted with the language
//! GtkSourceView detects from the file name and the token-based `assets/code-scheme.xml`, except
//! for large text (`presentation::syntax`); highlighting never changes the buffer text.

use std::cell::{Cell, RefCell};
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::rc::Rc;

use gtk::glib;
use gtk4 as gtk;
use sourceview5::prelude::*;

use super::widgets::{
    self, MessageKind, hbox, inline_message, label, set_accessible_description,
    set_accessible_label, space, vbox,
};
use crate::presentation::controller::Controller;
use crate::presentation::format;
use crate::presentation::model::{FileEncoding, Load, Side};
use crate::presentation::store::{Change, ViewerContent, ViewerMode};
use crate::presentation::syntax::{self, Highlight};
use crate::presentation::text_map::{self, TextPos, ViewLine, ViewLineKind};

const CODE_SCHEME: &str = include_str!("../assets/code-scheme.xml");
const SELECTION_HINT: &str = "Select text in the viewer to add feedback on exact bytes.";
const CODE_SCHEME_ID: &str = "lade-code";

/// The LADE code scheme. GtkSourceView reads schemes only from files, so the embedded scheme is
/// written to a private directory under the user's runtime directory and read by a scheme manager
/// that searches only there. `None` (plain text) if that fails.
fn code_scheme() -> Option<sourceview5::StyleScheme> {
    let dir = glib::user_runtime_dir().join("lade-desktop").join("styles");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .ok()?;
    let path = dir.join(format!("{CODE_SCHEME_ID}.xml"));
    if std::fs::read_to_string(&path).ok().as_deref() != Some(CODE_SCHEME) {
        let staged = dir.join(format!("{CODE_SCHEME_ID}.xml.{}", std::process::id()));
        std::fs::write(&staged, CODE_SCHEME).ok()?;
        std::fs::rename(&staged, &path).ok()?;
    }
    let manager = sourceview5::StyleSchemeManager::new();
    manager.set_search_path(&[dir.to_str()?]);
    manager.scheme(CODE_SCHEME_ID)
}

pub struct Viewer {
    pub root: gtk::Box,
    controller: Rc<Controller>,
    path_label: gtk::Label,
    counts: gtk::Box,
    diff_toggle: gtk::ToggleButton,
    file_toggle: gtk::ToggleButton,
    pub add_feedback: gtk::Button,
    selection_label: gtk::Label,
    /// Path, counts, mode and Add feedback: shown only while a file is open.
    toolbar: gtk::Box,
    stack: gtk::Stack,
    pub view: sourceview5::View,
    buffer: sourceview5::Buffer,
    message: gtk::Box,
    footer: gtk::Box,
    old_numbers: sourceview5::GutterRendererText,
    new_numbers: sourceview5::GutterRendererText,
    signs: sourceview5::GutterRendererText,
    lines: Rc<RefCell<Vec<ViewLine>>>,
    updating: Rc<Cell<bool>>,
    on_add_feedback: RefCell<Option<Rc<dyn Fn()>>>,
    /// Whether the last shown text is highlighted, plain, or plain because it is too large.
    highlight: Cell<Highlight>,
    /// A selection was made in this window: the how-to hint is no longer shown.
    hint_seen: Cell<bool>,
}

fn number_renderer(
    lines: &Rc<RefCell<Vec<ViewLine>>>,
    side: Side,
) -> sourceview5::GutterRendererText {
    let renderer = sourceview5::GutterRendererText::new();
    renderer.add_css_class("gutter-number");
    renderer.set_xalign(1.0);
    renderer.set_xpad(6);
    let lines = lines.clone();
    renderer.connect_query_data(move |r, _, line| {
        let text = lines
            .borrow()
            .get(line as usize)
            .and_then(|l| l.number(side))
            .map(|n| n.to_string())
            .unwrap_or_default();
        r.set_text(&text);
    });
    renderer
}

/// The stylesheet, read for the colour tokens the viewer sets from Rust.
const STYLE: &str = include_str!("../assets/style.css");

/// A colour token of the stylesheet (`@define-color <name> <value>;`). Text tags and gutter
/// markup cannot use CSS, so the diff fills, signs and separator ink are read from the same
/// tokens instead of repeating their values here.
fn token(name: &str) -> &'static str {
    STYLE
        .lines()
        .find_map(|line| {
            line.strip_prefix("@define-color ")?
                .strip_prefix(name)?
                .strip_prefix(' ')?
                .strip_suffix(';')
        })
        .unwrap_or("black")
}

fn sign_renderer(lines: &Rc<RefCell<Vec<ViewLine>>>) -> sourceview5::GutterRendererText {
    let renderer = sourceview5::GutterRendererText::new();
    renderer.add_css_class("gutter-sign");
    renderer.set_xalign(0.5);
    renderer.set_xpad(4);
    let lines = lines.clone();
    let sign = |ink: &str, sign: &str| {
        format!(
            "<span foreground=\"{}\" weight=\"bold\">{sign}</span>",
            token(ink)
        )
    };
    let (add, del) = (sign("add_ink", "+"), sign("del_ink", "−"));
    renderer.connect_query_data(move |r, _, line| {
        let markup = match lines.borrow().get(line as usize).map(|l| l.kind) {
            Some(ViewLineKind::Add) => add.as_str(),
            Some(ViewLineKind::Del) => del.as_str(),
            _ => " ",
        };
        r.set_markup(markup);
    });
    renderer
}

impl Viewer {
    pub fn new(controller: &Rc<Controller>) -> Rc<Self> {
        let root = vbox(0);
        root.add_css_class("viewer");

        let toolbar = hbox(space::GROUP);
        toolbar.add_css_class("viewer-toolbar");
        let path_label = widgets::line_label(
            "",
            &["mono", "viewer-path"],
            gtk::pango::EllipsizeMode::Start,
        );
        path_label.set_hexpand(true);
        toolbar.append(&path_label);
        let counts = hbox(0);
        toolbar.append(&counts);
        let modes = hbox(0);
        modes.add_css_class("segmented");
        let diff_toggle = gtk::ToggleButton::with_label("Diff");
        let file_toggle = gtk::ToggleButton::with_label("Full file");
        file_toggle.set_group(Some(&diff_toggle));
        diff_toggle.set_active(true);
        modes.append(&diff_toggle);
        modes.append(&file_toggle);
        set_accessible_label(&modes, "Viewer mode");
        toolbar.append(&modes);
        let add_feedback = widgets::button("Add feedback", &[]);
        add_feedback.set_sensitive(false);
        toolbar.append(&add_feedback);
        root.append(&toolbar);

        let selection_label = label("", &["meta", "selection-line"]);
        selection_label.set_wrap(false);
        selection_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        root.append(&selection_label);

        let buffer = sourceview5::Buffer::new(None);
        buffer.set_highlight_syntax(false);
        buffer.set_highlight_matching_brackets(false);
        buffer.set_style_scheme(code_scheme().as_ref());
        let table = buffer.tag_table();
        for (name, fill) in [
            ("add", "add_fill"),
            ("del", "del_fill"),
            ("sep", "fill_band"),
        ] {
            let tag = gtk::TextTag::builder()
                .name(name)
                .paragraph_background(token(fill))
                .build();
            if name == "sep" {
                tag.set_foreground(Some(token("ink_metadata")));
            }
            table.add(&tag);
        }
        // Highlighting tags are created lazily and take priority over earlier tags: keep the
        // hunk separator's own ink on top.
        buffer.connect_highlight_updated(|buffer, _, _| {
            let table = buffer.tag_table();
            if let Some(sep) = table.lookup("sep")
                && sep.priority() < table.size() - 1
            {
                sep.set_priority(table.size() - 1);
            }
        });
        let view = sourceview5::View::with_buffer(&buffer);
        view.set_editable(false);
        view.set_cursor_visible(true);
        view.set_monospace(true);
        view.set_show_line_numbers(false);
        view.set_highlight_current_line(false);
        view.set_tab_width(4);
        view.set_pixels_above_lines(2);
        view.set_pixels_below_lines(2);
        view.set_left_margin(8);
        view.set_wrap_mode(gtk::WrapMode::None);
        view.add_css_class("code-view");
        let lines: Rc<RefCell<Vec<ViewLine>>> = Rc::new(RefCell::new(Vec::new()));
        let gutter = sourceview5::prelude::ViewExt::gutter(&view, gtk::TextWindowType::Left);
        let old_numbers = number_renderer(&lines, Side::Old);
        let new_numbers = number_renderer(&lines, Side::New);
        let signs = sign_renderer(&lines);
        gutter.insert(&old_numbers, 0);
        gutter.insert(&new_numbers, 1);
        gutter.insert(&signs, 2);
        let digits = GutterRendererTextExt::measure(&old_numbers, "00000").0;
        old_numbers.set_width_request(digits + 12);
        new_numbers.set_width_request(digits + 12);
        signs.set_width_request(GutterRendererTextExt::measure(&signs, "−").0 + 12);

        // A usable minimum: when the window is short the work area scrolls as a whole rather
        // than squeezing the code to a few lines.
        let scroll = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .hexpand(true)
            .min_content_height(180)
            .overlay_scrolling(false)
            .build();
        scroll.add_css_class("code-scroll");
        let message = vbox(space::GROUP);
        message.add_css_class("viewer-message");
        message.set_valign(gtk::Align::Start);
        let stack = gtk::Stack::new();
        stack.add_named(&scroll, Some("text"));
        stack.add_named(&message, Some("message"));
        stack.set_vexpand(true);
        root.append(&stack);
        let footer = hbox(space::GROUP);
        footer.add_css_class("viewer-footer");
        root.append(&footer);

        let viewer = Rc::new(Self {
            root,
            controller: controller.clone(),
            path_label,
            counts,
            diff_toggle,
            file_toggle,
            add_feedback,
            selection_label,
            toolbar,
            stack,
            view,
            buffer,
            message,
            footer,
            old_numbers,
            new_numbers,
            signs,
            lines,
            updating: Rc::new(Cell::new(false)),
            on_add_feedback: RefCell::new(None),
            highlight: Cell::new(Highlight::NoLanguage),
            hint_seen: Cell::new(false),
        });
        viewer.connect();
        viewer.render();
        viewer
    }

    pub fn set_on_add_feedback(&self, handler: Rc<dyn Fn()>) {
        *self.on_add_feedback.borrow_mut() = Some(handler);
    }

    fn connect(self: &Rc<Self>) {
        for (toggle, mode) in [
            (&self.diff_toggle, ViewerMode::Diff),
            (&self.file_toggle, ViewerMode::FullFile),
        ] {
            let this = Rc::downgrade(self);
            toggle.connect_toggled(move |t| {
                if let Some(this) = this.upgrade()
                    && t.is_active()
                    && !this.updating.get()
                {
                    this.controller.set_viewer_mode(mode);
                }
            });
        }
        let this = Rc::downgrade(self);
        self.add_feedback.connect_clicked(move |_| {
            if let Some(this) = this.upgrade() {
                let handler = this.on_add_feedback.borrow().clone();
                if let Some(handler) = handler {
                    handler();
                }
            }
        });
        let this = Rc::downgrade(self);
        self.buffer.connect_mark_set(move |buffer, _, mark| {
            let Some(this) = this.upgrade() else { return };
            if this.updating.get() {
                return;
            }
            if mark == &buffer.get_insert() || mark == &buffer.selection_bound() {
                this.selection_changed();
            }
        });
    }

    fn selection_changed(&self) {
        let mapped = self.buffer.selection_bounds().map(|(a, b)| {
            let pos = |i: &gtk::TextIter| {
                TextPos::new(
                    usize::try_from(i.line()).unwrap_or(0),
                    usize::try_from(i.line_offset()).unwrap_or(0),
                )
            };
            text_map::map_selection(&self.lines.borrow(), pos(&a), pos(&b))
        });
        self.controller.set_viewer_selection(mapped);
    }

    pub fn refresh(&self, changes: &[Change]) {
        if changes
            .iter()
            .any(|c| matches!(c, Change::Viewer | Change::Selection))
        {
            self.render();
        } else if changes.iter().any(|c| matches!(c, Change::Changes(_))) {
            self.render_counts();
        }
        if changes.iter().any(|c| {
            matches!(
                c,
                Change::ViewerSelection | Change::Viewer | Change::Selection | Change::Detail(_)
            )
        }) {
            self.render_selection();
        }
    }

    fn show_message(&self, title: &str, body: &str) {
        widgets::clear(&self.message);
        self.message.set_valign(gtk::Align::Start);
        self.message
            .append(&widgets::heading(title, "empty-heading"));
        if !body.is_empty() {
            self.message.append(&label(body, &["secondary"]));
        }
        self.stack.set_visible_child_name("message");
    }

    fn set_lines(&self, lines: Vec<ViewLine>, mode: ViewerMode, side: Side, path: &str) {
        self.updating.set(true);
        let text = lines
            .iter()
            .map(|l| l.display.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        // Decide before the text goes in, so large text is never highlighted even briefly.
        let language = sourceview5::LanguageManager::default()
            .guess_language(Some(syntax::file_name(path)), None);
        let highlight = Highlight::decide(language.is_some(), text.len(), lines.len());
        self.highlight.set(highlight);
        self.buffer.set_highlight_syntax(false);
        self.buffer.set_language(None);
        self.buffer.set_text(&text);
        if highlight == Highlight::On {
            self.buffer.set_language(language.as_ref());
            self.buffer.set_highlight_syntax(true);
        }
        for (index, line) in lines.iter().enumerate() {
            let tag = match line.kind {
                ViewLineKind::Add => "add",
                ViewLineKind::Del => "del",
                ViewLineKind::Separator => "sep",
                _ => continue,
            };
            let Some(start) = self.buffer.iter_at_line(index as i32) else {
                continue;
            };
            let end = self
                .buffer
                .iter_at_line(index as i32 + 1)
                .unwrap_or_else(|| self.buffer.end_iter());
            self.buffer.apply_tag_by_name(tag, &start, &end);
        }
        *self.lines.borrow_mut() = lines;
        self.buffer.place_cursor(&self.buffer.start_iter());
        let diff = mode == ViewerMode::Diff;
        self.old_numbers.set_visible(diff || side == Side::Old);
        self.new_numbers.set_visible(diff || side == Side::New);
        self.signs.set_visible(diff);
        self.view.queue_draw();
        self.updating.set(false);
        self.stack.set_visible_child_name("text");
    }

    fn render(&self) {
        widgets::clear(&self.footer);
        self.footer.set_visible(false);
        let state = self.controller.store.state();
        let key = state.viewer.key.clone();
        let detail = state.selected_detail().cloned();
        let mode = state.selected_ui().map_or(ViewerMode::Diff, |u| u.mode);
        self.updating.set(true);
        match mode {
            ViewerMode::Diff => self.diff_toggle.set_active(true),
            ViewerMode::FullFile => self.file_toggle.set_active(true),
        }
        self.updating.set(false);
        let Some(key) = key.filter(|k| Some(&k.objective_id) == state.ui.selected.as_ref()) else {
            self.path_label.set_text("");
            widgets::clear(&self.counts);
            self.root.set_sensitive(true);
            drop(state);
            // No file: one empty state, without a mode toggle or Add feedback to act on.
            self.toolbar.set_visible(false);
            self.selection_label.set_visible(false);
            widgets::clear(&self.message);
            self.message.set_valign(gtk::Align::Fill);
            self.message.append(&widgets::empty_state(
                "Select a file to inspect",
                "Choose a file from the Change Set to see its diff or full content.",
                None,
                None,
            ));
            self.stack.set_visible_child_name("message");
            return;
        };
        self.toolbar.set_visible(true);
        self.selection_label.set_visible(true);
        self.path_label.set_text(&key.path.display);
        self.path_label.set_tooltip_text(Some(&key.path.display));
        let basis = format::basis_label(&key.basis, detail.as_ref());
        let content = state.viewer.content.clone();
        drop(state);
        self.render_counts();
        let a11y = match key.mode {
            ViewerMode::Diff => format!(
                "Diff of {}, {basis}. Added and removed lines are marked + and − in the gutter.",
                key.path.display
            ),
            ViewerMode::FullFile => format!(
                "Full file {}, {basis}, {}",
                key.path.display,
                format::side_label(key.side)
            ),
        };
        set_accessible_label(&self.view, &a11y);
        match content {
            ViewerContent::Empty | ViewerContent::Loading => {
                self.show_message("Loading…", &format!("{} · {basis}", key.path.display));
            }
            ViewerContent::Failed(message) => {
                widgets::clear(&self.message);
                self.message.set_valign(gtk::Align::Start);
                // A plain sentence ("Couldn't load diff. …"); the path is the toolbar's.
                self.message
                    .append(&inline_message(MessageKind::Error, &message, None));
                self.stack.set_visible_child_name("message");
                let retry = widgets::button("Retry", &[]);
                let controller = self.controller.clone();
                retry.connect_clicked(move |_| controller.refresh_changes());
                self.message.append(&retry);
            }
            ViewerContent::Diff(diff) => {
                if diff.binary {
                    self.show_message(
                        "Binary file — not shown as text",
                        "LADE does not render binary content. Its retained identity is listed in Evidence; open it outside LADE if you need to inspect it.",
                    );
                } else if diff.hunks.is_empty() {
                    self.show_message(
                        "No textual changes",
                        "Only the mode, type or name changed. Switch to Full file to read the content.",
                    );
                } else {
                    self.set_lines(
                        text_map::diff_lines(&diff),
                        ViewerMode::Diff,
                        Side::New,
                        &key.path.display,
                    );
                    self.show_highlight_note();
                    if diff.truncated {
                        let note = inline_message(
                            MessageKind::Warning,
                            "The service truncated this diff. Open Full file to page through every byte.",
                            None,
                        );
                        note.set_hexpand(true);
                        self.footer.append(&note);
                        self.footer.set_visible(true);
                    }
                }
            }
            ViewerContent::File(content) => {
                if content.encoding == FileEncoding::Binary {
                    self.show_message(
                        &format!(
                            "Binary file — {} bytes, not shown as text",
                            content.total_bytes
                        ),
                        &content.version_label,
                    );
                } else {
                    let value = self
                        .stack
                        .child_by_name("text")
                        .and_downcast::<gtk::ScrolledWindow>()
                        .map(|s| s.vadjustment().value());
                    self.set_lines(
                        text_map::file_lines(&content),
                        ViewerMode::FullFile,
                        content.side,
                        &key.path.display,
                    );
                    let shown = label(
                        &format!(
                            "{} · {} of {} bytes shown{}",
                            content.version_label,
                            content.loaded_bytes,
                            content.total_bytes,
                            if content.eof { " — complete" } else { "" }
                        ),
                        &["meta"],
                    );
                    shown.set_hexpand(true);
                    self.footer.append(&shown);
                    if let Some(note) = self.highlight.get().note() {
                        self.footer.append(&label(&note, &["meta"]));
                    }
                    if content.chunked && !content.eof {
                        self.footer.append(&label(
                            &format!(
                                "Large file ({}) — shown in bounded chunks",
                                crate::presentation::format::byte_size(content.total_bytes)
                            ),
                            &["meta"],
                        ));
                    }
                    if !content.eof {
                        let more = widgets::button("Load more", &[]);
                        widgets::set_focus_key(&more, "viewer:load-more");
                        let controller = self.controller.clone();
                        more.connect_clicked(move |_| controller.load_more());
                        self.footer.append(&more);
                    }
                    self.footer.set_visible(true);
                    if let (Some(value), Some(scroll)) = (
                        value,
                        self.stack
                            .child_by_name("text")
                            .and_downcast::<gtk::ScrolledWindow>(),
                    ) {
                        glib::idle_add_local_once(move || scroll.vadjustment().set_value(value));
                    }
                    // A followed content search result: select its exact bytes (queued after
                    // the scroll restore) so the selection maps to a feedback reference.
                    if let Some((start, end)) =
                        self.controller.take_reveal(&key, content.loaded_bytes)
                    {
                        let (view, buffer, lines) =
                            (self.view.clone(), self.buffer.clone(), self.lines.clone());
                        let side = content.side;
                        glib::idle_add_local_once(move || {
                            let lines = lines.borrow();
                            let iter = |byte| {
                                let pos = text_map::position_at(&lines, side, byte)?;
                                buffer.iter_at_line_offset(
                                    i32::try_from(pos.line).ok()?,
                                    i32::try_from(pos.char_offset).ok()?,
                                )
                            };
                            if let (Some(from), Some(to)) = (iter(start), iter(end)) {
                                buffer.select_range(&from, &to);
                                view.scroll_to_mark(&buffer.get_insert(), 0.1, true, 0.0, 0.3);
                            }
                        });
                    }
                }
            }
        }
    }

    /// States in the footer that large text is shown without highlighting.
    fn show_highlight_note(&self) {
        if let Some(note) = self.highlight.get().note() {
            self.footer.append(&label(&note, &["meta"]));
            self.footer.set_visible(true);
        }
    }

    fn render_counts(&self) {
        widgets::clear(&self.counts);
        let state = self.controller.store.state();
        let Some(key) = state.viewer.key.as_ref() else {
            return;
        };
        if let Some(file) = state
            .changes
            .get(&key.objective_id)
            .and_then(Load::loaded)
            .and_then(|l| l.files.iter().find(|f| f.path == key.path))
        {
            self.counts
                .append(&widgets::counts(file.added, file.removed));
        }
    }

    fn render_selection(&self) {
        let state = self.controller.store.state();
        let detail = state.selected_detail();
        let has_session = detail.is_some_and(|d| d.session.is_some());
        let selection = state.selected_ui().and_then(|u| u.selection.clone());
        // The how-to hint shows until the first selection in this window; afterwards the line
        // appears only for a selection or a reason it cannot be used. Add feedback always keeps
        // the hint as its tooltip and accessible description.
        if matches!(selection, Some(Ok(_))) {
            self.hint_seen.set(true);
        }
        let (text, enabled) = match selection {
            None => (SELECTION_HINT.to_owned(), false),
            Some(Err(reason)) => (reason, false),
            Some(Ok(reference)) => (
                format!("Selection: {}", format::reference_label(&reference, detail)),
                has_session,
            ),
        };
        // The line keeps its height (style.css) so the code never moves under the pointer.
        let shown = if text == SELECTION_HINT && self.hint_seen.get() {
            ""
        } else {
            text.as_str()
        };
        self.selection_label.set_text(shown);
        self.selection_label
            .set_tooltip_text((!shown.is_empty()).then_some(shown));
        self.add_feedback.set_sensitive(enabled);
        let reason = if enabled {
            "Create a feedback draft for the selected bytes".to_owned()
        } else if !has_session {
            "No Agent Session to address — start the agent first".to_owned()
        } else {
            text.clone()
        };
        self.add_feedback.set_tooltip_text(Some(&reason));
        set_accessible_description(&self.add_feedback, &reason);
    }
}

#[cfg(test)]
mod tests {
    use super::token;

    #[test]
    fn diff_colours_are_the_stylesheet_tokens() {
        for name in [
            "add_ink",
            "del_ink",
            "add_fill",
            "del_fill",
            "fill_band",
            "ink_metadata",
        ] {
            let value = token(name);
            assert_ne!(value, "black", "{name} is a style.css token");
            assert!(
                gtk4::gdk::RGBA::parse(value).is_ok(),
                "{name} = {value} parses as a colour"
            );
        }
        assert_eq!(token("add_ink"), "#146C43");
        assert_eq!(token("missing_token"), "black");
    }
}
