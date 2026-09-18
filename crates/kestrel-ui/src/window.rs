//! The widgets.
//!
//! Everything here runs on GTK's thread and nothing here blocks. Work that
//! could block goes down the command channel to the connection half, and comes
//! back as events; this file only ever draws what it is told.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use kestrel_ui::event::{AppEvent, BufferId, Line, LineKind, SERVER_BUFFER, UiCommand};
use tokio::sync::mpsc;

/// What the server buffer is called in the sidebar, since its id is empty.
const SERVER_LABEL: &str = "server";

/// One conversation's worth of state.
struct Buffer {
    text: gtk::TextBuffer,
    /// A mark pinned to the end, so new lines can be scrolled to.
    end: gtk::TextMark,
    members: Vec<String>,
    topic: String,
    unread: bool,
}

/// Everything the interface remembers.
struct State {
    buffers: BTreeMap<BufferId, Buffer>,
    /// Sidebar order, which is also the row order.
    order: Vec<BufferId>,
    current: BufferId,
    nick: String,
}

/// The window and the things that change inside it.
#[derive(Clone)]
pub struct Window {
    view: gtk::TextView,
    entry: gtk::Entry,
    topic: gtk::Label,
    sidebar: gtk::ListBox,
    members: gtk::ListBox,
    members_pane: gtk::Widget,
    tags: gtk::TextTagTable,
    state: Rc<RefCell<State>>,
    commands: mpsc::UnboundedSender<UiCommand>,
    /// Set while the sidebar is being changed from code, so the selection
    /// handler does not treat its own work as a click from the user.
    selecting: Rc<RefCell<bool>>,
}

impl Window {
    /// Build the window and show it.
    pub fn build(
        app: &gtk::Application,
        commands: mpsc::UnboundedSender<UiCommand>,
    ) -> (Self, gtk::ApplicationWindow) {
        let tags = gtk::TextTagTable::new();
        add_tags(&tags);

        let view = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(8)
            .right_margin(8)
            .top_margin(4)
            .bottom_margin(4)
            .monospace(true)
            .build();

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();

        let topic = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .single_line_mode(true)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();

        let entry = gtk::Entry::builder()
            .placeholder_text("Say something, or /help")
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .build();

        let sidebar = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        let sidebar_pane = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(160)
            .child(&sidebar)
            .build();

        let members = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        let members_pane = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(140)
            .child(&members)
            .build();

        let centre = gtk::Box::new(gtk::Orientation::Vertical, 0);
        centre.append(&topic);
        centre.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        centre.append(&scroller);
        centre.append(&entry);
        centre.set_hexpand(true);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.append(&sidebar_pane);
        row.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        row.append(&centre);
        row.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        row.append(&members_pane);

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Kestrel")
            .default_width(980)
            .default_height(620)
            .child(&row)
            .build();

        let ui = Self {
            view,
            entry,
            topic,
            sidebar,
            members,
            members_pane: members_pane.upcast(),
            tags,
            state: Rc::new(RefCell::new(State {
                buffers: BTreeMap::new(),
                order: Vec::new(),
                current: SERVER_BUFFER.to_owned(),
                nick: String::new(),
            })),
            commands,
            selecting: Rc::new(RefCell::new(false)),
        };

        // The server buffer always exists: it is where anything that belongs
        // to no conversation goes, including the reason a connection failed.
        ui.ensure_buffer(SERVER_BUFFER);
        ui.show_buffer(SERVER_BUFFER);
        ui.connect_signals();

        window.present();
        (ui, window)
    }

    fn connect_signals(&self) {
        let ui = self.clone();
        self.entry.connect_activate(move |entry| {
            let text = entry.text().to_string();
            if text.is_empty() {
                return;
            }
            entry.set_text("");
            let buffer = ui.state.borrow().current.clone();
            // An unbounded sender never blocks, which is why it is safe to use
            // one straight from a GTK handler.
            let _ = ui.commands.send(UiCommand::Input { buffer, text });
        });

        let ui = self.clone();
        self.sidebar.connect_row_selected(move |_, row| {
            if *ui.selecting.borrow() {
                return;
            }
            let Some(row) = row else { return };
            let index = usize::try_from(row.index()).unwrap_or(0);
            let Some(buffer) = ui.state.borrow().order.get(index).cloned() else {
                return;
            };
            ui.show_buffer(&buffer);
        });
    }

    /// Apply one event from the connection.
    pub fn handle(&self, event: AppEvent) {
        match event {
            AppEvent::Connecting { server } => {
                self.append(
                    SERVER_BUFFER,
                    &Line::status(format!("connecting to {server}")),
                );
            }
            AppEvent::Registered { nick } | AppEvent::NickChanged { nick } => {
                self.state.borrow_mut().nick = nick;
                self.retitle();
            }
            AppEvent::Line { buffer, line } => self.append(&buffer, &line),
            AppEvent::OpenBuffer { buffer } => {
                self.ensure_buffer(&buffer);
                self.show_buffer(&buffer);
            }
            AppEvent::CloseBuffer { buffer } => self.close_buffer(&buffer),
            AppEvent::Roster { buffer, members } => {
                if let Some(entry) = self.state.borrow_mut().buffers.get_mut(&buffer) {
                    entry.members = members;
                }
                if self.state.borrow().current == buffer {
                    self.redraw_members();
                }
            }
            AppEvent::Topic { buffer, topic } => {
                if let Some(entry) = self.state.borrow_mut().buffers.get_mut(&buffer) {
                    entry.topic = topic;
                }
                if self.state.borrow().current == buffer {
                    self.redraw_topic();
                }
            }
            AppEvent::Disconnected { reason } => {
                self.append(
                    SERVER_BUFFER,
                    &Line::error(format!("disconnected: {reason}")),
                );
                self.entry.set_sensitive(false);
                self.entry
                    .set_placeholder_text(Some("disconnected — close the window to leave"));
            }
        }
    }

    /// Create a buffer if it does not exist yet.
    fn ensure_buffer(&self, id: &str) {
        {
            let state = self.state.borrow();
            if state.buffers.contains_key(id) {
                return;
            }
        }

        let text = gtk::TextBuffer::new(Some(&self.tags));
        // Right gravity, so the mark stays after everything inserted later and
        // scrolling to it always lands at the bottom.
        let end = text.create_mark(None, &text.end_iter(), false);

        let mut state = self.state.borrow_mut();
        state.buffers.insert(
            id.to_owned(),
            Buffer {
                text,
                end,
                members: Vec::new(),
                topic: String::new(),
                unread: false,
            },
        );
        drop(state);

        self.rebuild_sidebar();
    }

    /// Bring a buffer forward.
    fn show_buffer(&self, id: &str) {
        self.ensure_buffer(id);

        {
            let mut state = self.state.borrow_mut();
            id.clone_into(&mut state.current);
            if let Some(entry) = state.buffers.get_mut(id) {
                entry.unread = false;
                self.view.set_buffer(Some(&entry.text));
            }
        }

        self.rebuild_sidebar();
        self.redraw_members();
        self.redraw_topic();
        self.retitle();
        self.scroll_to_end();
        self.entry.grab_focus();
    }

    fn close_buffer(&self, id: &str) {
        if id.is_empty() {
            return;
        }
        let fallback = {
            let mut state = self.state.borrow_mut();
            state.buffers.remove(id);
            state.order.retain(|name| name != id);
            (state.current == id).then(|| SERVER_BUFFER.to_owned())
        };

        self.rebuild_sidebar();
        if let Some(fallback) = fallback {
            self.show_buffer(&fallback);
        }
    }

    /// Put a line at the end of a buffer.
    fn append(&self, id: &str, line: &Line) {
        self.ensure_buffer(id);

        let visible = self.state.borrow().current == id;
        {
            let mut state = self.state.borrow_mut();
            let Some(entry) = state.buffers.get_mut(id) else {
                return;
            };
            if !visible {
                entry.unread = true;
            }

            let text = &entry.text;
            let mut at = text.end_iter();
            if text.char_count() > 0 {
                text.insert(&mut at, "\n");
            }

            if let Some(who) = &line.who {
                let rendered = match line.kind {
                    LineKind::Action => format!("* {who} "),
                    LineKind::Notice => format!("-{who}- "),
                    _ => format!("<{who}> "),
                };
                text.insert_with_tags_by_name(&mut at, &rendered, &[nick_tag(line.kind)]);
            }
            text.insert_with_tags_by_name(&mut at, &line.text, &[body_tag(line.kind)]);
        }

        if !visible {
            self.rebuild_sidebar();
            return;
        }
        self.scroll_to_end();
    }

    fn scroll_to_end(&self) {
        let state = self.state.borrow();
        let Some(entry) = state.buffers.get(&state.current) else {
            return;
        };
        // Moved rather than trusted to stay put: a mark only tracks the end if
        // nothing has been inserted at exactly that position with the other
        // gravity, and this costs nothing.
        entry.text.move_mark(&entry.end, &entry.text.end_iter());
        self.view.scroll_to_mark(&entry.end, 0.0, true, 0.0, 1.0);
    }

    fn rebuild_sidebar(&self) {
        let rows: Vec<(BufferId, String, bool)> = {
            let mut state = self.state.borrow_mut();
            // Channels and conversations sort together under the server, which
            // stays first because it is where errors land.
            let mut names: Vec<BufferId> = state.buffers.keys().cloned().collect();
            names.sort_by(|a, b| {
                (a != SERVER_BUFFER, a.clone()).cmp(&(b != SERVER_BUFFER, b.clone()))
            });
            state.order.clone_from(&names);

            names
                .into_iter()
                .map(|name| {
                    let unread = state.buffers.get(&name).is_some_and(|b| b.unread);
                    let label = if name.is_empty() {
                        SERVER_LABEL.to_owned()
                    } else {
                        name.clone()
                    };
                    (name, label, unread)
                })
                .collect()
        };

        *self.selecting.borrow_mut() = true;
        while let Some(child) = self.sidebar.first_child() {
            self.sidebar.remove(&child);
        }

        let current = self.state.borrow().current.clone();
        let mut selected = None;
        for (index, (id, label, unread)) in rows.iter().enumerate() {
            let text = if *unread {
                format!("• {label}")
            } else {
                label.clone()
            };
            let widget = gtk::Label::builder()
                .label(&text)
                .xalign(0.0)
                .margin_start(8)
                .margin_end(8)
                .margin_top(4)
                .margin_bottom(4)
                .build();
            self.sidebar.append(&widget);
            if id == &current {
                selected = Some(index);
            }
        }

        if let Some(index) = selected
            && let Some(row) = self.sidebar.row_at_index(i32::try_from(index).unwrap_or(0))
        {
            self.sidebar.select_row(Some(&row));
        }
        *self.selecting.borrow_mut() = false;
    }

    fn redraw_members(&self) {
        while let Some(child) = self.members.first_child() {
            self.members.remove(&child);
        }

        let state = self.state.borrow();
        let members = state
            .buffers
            .get(&state.current)
            .map(|b| b.members.clone())
            .unwrap_or_default();

        // A conversation with one person has no member list worth the space.
        let is_channel = state.current.starts_with('#') || state.current.starts_with('&');
        self.members_pane.set_visible(is_channel);
        if !is_channel {
            return;
        }

        for member in members {
            self.members.append(
                &gtk::Label::builder()
                    .label(&member)
                    .xalign(0.0)
                    .margin_start(8)
                    .margin_end(8)
                    .margin_top(2)
                    .margin_bottom(2)
                    .build(),
            );
        }
    }

    fn redraw_topic(&self) {
        let state = self.state.borrow();
        let topic = state
            .buffers
            .get(&state.current)
            .map(|b| b.topic.clone())
            .unwrap_or_default();
        let name = if state.current.is_empty() {
            SERVER_LABEL
        } else {
            &state.current
        };
        self.topic.set_text(&if topic.is_empty() {
            name.to_owned()
        } else {
            format!("{name} — {topic}")
        });
    }

    fn retitle(&self) {
        let state = self.state.borrow();
        let Some(window) = self.view.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let title = if state.nick.is_empty() {
            "Kestrel".to_owned()
        } else {
            format!("Kestrel — {}", state.nick)
        };
        window.set_title(Some(&title));
    }

    /// Ask the connection to leave.
    pub fn quit(&self) {
        let _ = self.commands.send(UiCommand::Quit {
            reason: "kestrel".to_owned(),
        });
    }
}

/// Which tag paints the name at the start of a line.
fn nick_tag(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Action => "action",
        LineKind::Notice => "notice",
        LineKind::Own => "own-nick",
        _ => "nick",
    }
}

/// Which tag paints the rest of it.
fn body_tag(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Action => "action",
        LineKind::Notice => "notice",
        LineKind::Status => "status",
        LineKind::Error => "error",
        LineKind::Message | LineKind::Own => "body",
    }
}

/// Colours, defined once and shared by every buffer.
///
/// Set as foreground colours rather than CSS classes because they apply to
/// ranges of text inside one widget, which CSS cannot address.
fn add_tags(table: &gtk::TextTagTable) {
    let tag = |name: &str, colour: &str, bold: bool| {
        let tag = gtk::TextTag::builder()
            .name(name)
            .foreground_rgba(&parse_colour(colour))
            .build();
        if bold {
            tag.set_weight(700);
        }
        table.add(&tag);
    };

    tag("nick", "#7aa2f7", true);
    tag("own-nick", "#9ece6a", true);
    tag("body", "#c0caf5", false);
    tag("status", "#7f849c", false);
    tag("error", "#f7768e", false);
    tag("notice", "#e0af68", false);
    tag("action", "#bb9af7", false);
}

fn parse_colour(hex: &str) -> gdk::RGBA {
    hex.parse::<gdk::RGBA>().unwrap_or(gdk::RGBA::WHITE)
}

/// Pump events from the connection into the window.
///
/// `spawn_future_local` rather than a thread: the future stays on GTK's thread,
/// so it may hold widgets, which are `!Send` and could not cross to another.
pub fn pump(ui: Window, events: async_channel::Receiver<AppEvent>) {
    glib::spawn_future_local(async move {
        while let Ok(event) = events.recv().await {
            ui.handle(event);
        }
    });
}
