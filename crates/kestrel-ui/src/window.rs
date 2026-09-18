//! The widgets.
//!
//! Everything here runs on GTK's thread and nothing here blocks. Work that
//! could block goes down the command channel to the connection half, and comes
//! back as events; this file only ever draws what it is told.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use kestrel_media::gstreamer::prelude::{ElementExt, PadExtManual};
use kestrel_net::ConnectConfig;
use kestrel_session::SessionConfig;
use kestrel_ui::connection::CallOptions;
use kestrel_ui::event::{AppEvent, BufferId, CallAction, Line, LineKind, SERVER_BUFFER, UiCommand};
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
    /// What to call the server buffer: the address we were pointed at.
    server: String,
    /// Whether there is still a connection behind this window.
    connected: bool,
    /// Where this window was last pointed, so it can be pointed there again.
    last: Option<(ConnectConfig, SessionConfig)>,
    /// How calls from this window are placed.
    call_options: CallOptions,
    /// The pictures on screen, and the conversation each belongs to.
    ///
    /// A call in a private conversation has no business covering a channel
    /// somebody switched to in order to read it.
    pictures: Vec<(BufferId, gtk::Widget)>,
    /// Somebody is ringing.
    ringing: bool,
    /// A call is in progress.
    in_call: bool,
}

/// The window and the things that change inside it.
#[derive(Clone)]
pub struct Window {
    view: gtk::TextView,
    entry: gtk::Entry,
    topic: gtk::Label,
    /// Where a call's video is drawn.
    videos: gtk::Box,
    sidebar: gtk::ListBox,
    members: gtk::ListBox,
    members_pane: gtk::Widget,
    tags: gtk::TextTagTable,
    state: Rc<RefCell<State>>,
    /// Swapped out when the window is given a new connection, so every clone
    /// of this handle starts talking to the new one at the same moment.
    commands: Rc<RefCell<mpsc::UnboundedSender<UiCommand>>>,
    /// Set while the sidebar is being changed from code, so the selection
    /// handler does not treat its own work as a click from the user.
    selecting: Rc<RefCell<bool>>,
}

impl Window {
    /// Build the window and show it.
    // Laying out a window is a long straight line of widgets; splitting it
    // would scatter the layout rather than clarify it.
    #[allow(clippy::too_many_lines)]
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

        // Where a call's video goes. Hidden until there is any, so a text
        // client does not permanently reserve a third of its own window.
        let videos = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        videos.set_homogeneous(false);
        videos.set_margin_start(6);
        videos.set_margin_end(6);
        videos.set_margin_top(6);
        videos.set_height_request(240);
        videos.set_visible(false);

        let centre = gtk::Box::new(gtk::Orientation::Vertical, 0);
        centre.append(&topic);
        centre.append(&videos);
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

        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.append(&gtk::PopoverMenuBar::from_model(Some(&menu_model())));
        outer.append(&row);

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Kestrel")
            .default_width(980)
            .default_height(620)
            .child(&outer)
            .build();

        let ui = Self {
            view,
            entry,
            topic,
            videos,
            sidebar,
            members,
            members_pane: members_pane.upcast(),
            tags,
            state: Rc::new(RefCell::new(State {
                buffers: BTreeMap::new(),
                order: Vec::new(),
                current: SERVER_BUFFER.to_owned(),
                nick: String::new(),
                server: String::new(),
                connected: true,
                last: None,
                call_options: CallOptions::default(),
                pictures: Vec::new(),
                ringing: false,
                in_call: false,
            })),
            commands: Rc::new(RefCell::new(commands)),
            selecting: Rc::new(RefCell::new(false)),
        };

        // The server buffer always exists: it is where anything that belongs
        // to no conversation goes, including the reason a connection failed.
        ui.install_actions(app, &window);
        ui.ensure_buffer(SERVER_BUFFER);
        ui.show_buffer(SERVER_BUFFER);
        ui.connect_signals();

        // Tab is how focus moves, and the focus handling sees it before any
        // accelerator does. Watching for it on the way down is the only way to
        // claim it, so the accelerators above are for the menu to display and
        // this is what actually works.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let cycling = ui.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if key != gdk::Key::Tab && key != gdk::Key::ISO_Left_Tab {
                return glib::Propagation::Proceed;
            }
            if !modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
                return glib::Propagation::Proceed;
            }
            let back =
                modifiers.contains(gdk::ModifierType::SHIFT_MASK) || key == gdk::Key::ISO_Left_Tab;
            cycling.cycle_buffer(if back { -1 } else { 1 });
            glib::Propagation::Stop
        });
        window.add_controller(keys);

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
            let _ = ui.commands.borrow().send(UiCommand::Input { buffer, text });
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
                {
                    let mut state = self.state.borrow_mut();
                    state.server.clone_from(&server);
                    state.connected = true;
                }
                self.append(
                    SERVER_BUFFER,
                    &Line::status(format!("connecting to {server}")),
                );
                self.rebuild_sidebar();
                self.redraw_topic();
                self.update_actions();
            }
            AppEvent::ServerInfo { name, version } => {
                if let Some(entry) = self.state.borrow_mut().buffers.get_mut(SERVER_BUFFER) {
                    entry.topic = format!("{name} — {version}");
                }
                self.redraw_topic();
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
            AppEvent::CallState { ringing, active } => {
                {
                    let mut state = self.state.borrow_mut();
                    state.ringing = ringing;
                    state.in_call = active;
                }
                if !active {
                    self.clear_video();
                }
                self.update_actions();
            }
            AppEvent::SelfViewWanted { target } => self.show_video("you", &target),
            AppEvent::VideoWanted { peer, target } => self.show_video(&peer, &target),
            AppEvent::Disconnected { reason } => {
                self.state.borrow_mut().connected = false;
                self.append(
                    SERVER_BUFFER,
                    &Line::error(format!("disconnected: {reason}")),
                );
                self.entry.set_sensitive(false);
                self.entry
                    .set_placeholder_text(Some("disconnected — Server ▸ Reconnect"));
                self.update_actions();
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

    /// Move to the next buffer along, wrapping at the end.
    ///
    /// `step` is 1 for forwards and -1 for back. Wrapping rather than stopping
    /// because a list of four buffers is not somewhere you want to notice an
    /// edge.
    fn cycle_buffer(&self, step: isize) {
        let next = {
            let state = self.state.borrow();
            if state.order.len() < 2 {
                return;
            }
            let at = state
                .order
                .iter()
                .position(|name| name == &state.current)
                .unwrap_or(0);
            let count = isize::try_from(state.order.len()).unwrap_or(1);
            let index = (isize::try_from(at).unwrap_or(0) + step).rem_euclid(count);
            state
                .order
                .get(usize::try_from(index).unwrap_or(0))
                .cloned()
        };

        if let Some(next) = next {
            self.show_buffer(&next);
        }
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
        self.redraw_video();
        self.redraw_topic();
        self.retitle();
        self.update_actions();
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
            let server_label = if state.server.is_empty() {
                SERVER_LABEL.to_owned()
            } else {
                state.server.clone()
            };
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
                        server_label.clone()
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

            // Channels and conversations sit under the server they belong to,
            // one character in, so the list reads as a tree rather than a pile
            // once there is more than one connection in the window.
            let indent = if id.is_empty() { 8 } else { 20 };
            let widget = gtk::Label::builder()
                .label(&text)
                .xalign(0.0)
                .margin_start(indent)
                .margin_end(8)
                .margin_top(4)
                .margin_bottom(4)
                .build();

            let menu = gtk::PopoverMenu::from_model(Some(&buffer_menu(id)));
            menu.set_parent(&widget);
            menu.set_has_arrow(false);
            menu.set_halign(gtk::Align::Start);

            let click = gtk::GestureClick::new();
            click.set_button(gdk::BUTTON_SECONDARY);
            let showing = menu.clone();
            click.connect_pressed(move |_, _, x, y| {
                showing.set_pointing_to(Some(&gdk::Rectangle::new(at(x), at(y), 1, 1)));
                showing.popup();
            });
            widget.add_controller(click);

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
            let label = gtk::Label::builder()
                .label(&member)
                .xalign(0.0)
                .margin_start(8)
                .margin_end(8)
                .margin_top(2)
                .margin_bottom(2)
                .build();

            // The prefix says what they can do in the channel, not who they
            // are; everything offered here is addressed to the person.
            let nick = member
                .trim_start_matches(['~', '&', '@', '%', '+'])
                .to_owned();

            let menu = gtk::PopoverMenu::from_model(Some(&member_menu(&nick)));
            menu.set_parent(&label);
            menu.set_has_arrow(false);
            menu.set_halign(gtk::Align::Start);

            let click = gtk::GestureClick::new();
            click.set_button(gdk::BUTTON_SECONDARY);
            let showing = menu.clone();
            click.connect_pressed(move |_, _, x, y| {
                showing.set_pointing_to(Some(&gdk::Rectangle::new(at(x), at(y), 1, 1)));
                showing.popup();
            });
            label.add_controller(click);

            self.members.append(&label);
        }
    }

    fn redraw_topic(&self) {
        let state = self.state.borrow();
        let topic = state
            .buffers
            .get(&state.current)
            .map(|b| b.topic.clone())
            .unwrap_or_default();
        let name = if !state.current.is_empty() {
            state.current.clone()
        } else if state.server.is_empty() {
            SERVER_LABEL.to_owned()
        } else {
            state.server.clone()
        };
        self.topic.set_text(&if topic.is_empty() {
            name
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

    /// Wire the menu up.
    ///
    /// Every entry goes through the same command parsing as typing the
    /// equivalent slash command, so there is one implementation of what
    /// joining a channel means rather than two that can drift apart.
    // One block per action, which is long but entirely flat.
    #[allow(clippy::too_many_lines)]
    fn install_actions(&self, app: &gtk::Application, window: &gtk::ApplicationWindow) {
        self.add_prompt_action(window, "join", "Join Channel", "Channel", "#", |name| {
            format!("/join {name}")
        });
        self.add_prompt_action(
            window,
            "query",
            "Open Conversation",
            "Nickname",
            "",
            |who| format!("/query {who}"),
        );
        self.add_prompt_action(window, "nick", "Change Nickname", "Nickname", "", |nick| {
            format!("/nick {nick}")
        });
        self.add_prompt_action(window, "topic", "Set Topic", "Topic", "", |topic| {
            format!("/topic {topic}")
        });

        // Captured rather than looked up: the window this action lives on may
        // well be closed by the time a second connection is wanted, and the
        // application outlives all of its windows.
        let application = app.clone();
        let action = gio::SimpleAction::new("connect", None);
        let from = self.clone();
        action.connect_activate(move |_, _| {
            crate::connect::show(&application, Some(from.clone()), &from.call_options());
        });
        window.add_action(&action);

        // Redialling where we already were, which after a dropped connection
        // is almost always what is wanted and needs no form to say so.
        self.add_action(window, "reconnect", |ui| {
            let Some((connect, session)) = ui.last_connection() else {
                return;
            };
            if let Err(error) = ui.redial(connect, session) {
                ui.append(
                    SERVER_BUFFER,
                    &Line::error(format!("could not reconnect: {error:#}")),
                );
            }
        });

        // Actions that carry who they are about, for the member list's own
        // menu. A plain action cannot say which person was clicked.
        // Closing a conversation: parting a channel if it is one, and simply
        // forgetting the buffer if it is not.
        {
            let action = gio::SimpleAction::new("close-buffer", Some(glib::VariantTy::STRING));
            let ui = self.clone();
            action.connect_activate(move |_, parameter| {
                let Some(name) = parameter.and_then(glib::Variant::str) else {
                    return;
                };
                if name.starts_with('#') || name.starts_with('&') {
                    ui.run(&format!("/part {name}"));
                }
                ui.close_buffer(name);
            });
            window.add_action(&action);
        }

        for name in ["call-nick", "query-nick", "whois-nick"] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let ui = self.clone();
            let which = name;
            action.connect_activate(move |_, parameter| {
                let Some(nick) = parameter.and_then(glib::Variant::str) else {
                    return;
                };
                match which {
                    "call-nick" => ui.call(CallAction::Start(nick.to_owned())),
                    "query-nick" => ui.run(&format!("/query {nick}")),
                    _ => ui.run(&format!("/whois {nick}")),
                }
            });
            window.add_action(&action);
        }

        self.add_action(window, "devices", Self::choose_devices);
        self.add_action(window, "next-buffer", |ui| ui.cycle_buffer(1));
        self.add_action(window, "previous-buffer", |ui| ui.cycle_buffer(-1));

        self.add_action(window, "answer", |ui| ui.call(CallAction::Answer));
        self.add_action(window, "reject", |ui| ui.call(CallAction::Reject));
        self.add_action(window, "hangup", |ui| ui.call(CallAction::HangUp));
        self.add_action(window, "verify", |ui| ui.call(CallAction::Verify));
        // Its own action rather than a slash command: a call is not text, and
        // routing it through the message parser would only invent a syntax.
        let action = gio::SimpleAction::new("call", None);
        let ui = self.clone();
        action.connect_activate(move |_, _| {
            // Whoever is on screen is almost always who you mean to call.
            let initial = ui.state.borrow().current.clone();
            let target = ui.clone();
            ui.prompt(
                "Start a Call",
                "Who, or which channel",
                &initial,
                move |who| {
                    target.call(CallAction::Start(who));
                },
            );
        });
        window.add_action(&action);

        self.add_action(window, "names", |ui| ui.run("/names"));
        self.add_action(window, "part", |ui| ui.run("/part"));
        self.add_action(window, "disconnect", Self::quit);
        self.add_action(window, "about", Self::show_about);

        // Anything the menu names but nothing above installed becomes a
        // disabled placeholder. That is how the call entries behave today:
        // visible, so the shape of what is coming is apparent, and plainly
        // unavailable rather than silently doing nothing when clicked.
        for name in ACTIONS {
            if window.lookup_action(name).is_none() {
                let action = gio::SimpleAction::new(name, None);
                action.set_enabled(false);
                window.add_action(&action);
            }
        }

        for (action, keys) in [
            ("win.next-buffer", "<Ctrl>Tab"),
            ("win.previous-buffer", "<Ctrl><Shift>Tab"),
            ("win.connect", "<Ctrl>n"),
            ("win.join", "<Ctrl>j"),
            ("win.query", "<Ctrl>q"),
            ("win.part", "<Ctrl>w"),
            ("win.disconnect", "<Ctrl>Q"),
        ] {
            app.set_accels_for_action(action, &[keys]);
        }

        self.update_actions();
    }

    /// Add an action that does something immediately.
    fn add_action(
        &self,
        window: &gtk::ApplicationWindow,
        name: &str,
        run: impl Fn(&Self) + 'static,
    ) {
        let action = gio::SimpleAction::new(name, None);
        let ui = self.clone();
        action.connect_activate(move |_, _| run(&ui));
        window.add_action(&action);
    }

    /// Add an action that asks for something first.
    fn add_prompt_action(
        &self,
        window: &gtk::ApplicationWindow,
        name: &str,
        title: &str,
        label: &str,
        initial: &str,
        to_command: impl Fn(&str) -> String + 'static,
    ) {
        let action = gio::SimpleAction::new(name, None);
        let ui = self.clone();
        let title = title.to_owned();
        let label = label.to_owned();
        let initial = initial.to_owned();
        // Shared rather than borrowed: the answer arrives long after this
        // handler has returned, so what turns it into a command has to outlive
        // the handler that asked the question.
        let to_command = std::rc::Rc::new(to_command);
        action.connect_activate(move |_, _| {
            let target = ui.clone();
            let to_command = std::rc::Rc::clone(&to_command);
            ui.prompt(&title, &label, &initial, move |answer| {
                target.run(&to_command(&answer));
            });
        });
        window.add_action(&action);
    }

    /// Grey out what does not apply to the buffer on screen.
    fn update_actions(&self) {
        let Some(window) = self.window() else { return };
        let current = self.state.borrow().current.clone();
        let in_channel = current.starts_with('#') || current.starts_with('&');

        let (connected, ringing, in_call, has_last) = {
            let state = self.state.borrow();
            (
                state.connected,
                state.ringing,
                state.in_call,
                state.last.is_some(),
            )
        };
        for (name, enabled) in [
            ("part", in_channel && connected),
            ("topic", in_channel && connected),
            ("names", in_channel && connected),
            ("reconnect", !connected && has_last),
            ("call", connected && !in_call),
            ("answer", connected && ringing),
            ("reject", connected && ringing),
            ("hangup", connected && in_call),
            ("verify", connected && in_call),
        ] {
            if let Some(action) = window
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(enabled);
            }
        }
    }

    /// Ask a one-line question, then do something with the answer.
    fn prompt(&self, title: &str, label: &str, initial: &str, accept: impl Fn(String) + 'static) {
        let Some(parent) = self.window() else { return };

        let entry = gtk::Entry::builder().text(initial).hexpand(true).build();
        entry.set_position(-1);

        let cancel = gtk::Button::with_label("Cancel");
        let confirm = gtk::Button::with_label("OK");
        confirm.add_css_class("suggested-action");

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        buttons.set_halign(gtk::Align::End);
        buttons.append(&cancel);
        buttons.append(&confirm);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.append(&gtk::Label::builder().label(label).xalign(0.0).build());
        content.append(&entry);
        content.append(&buttons);

        let dialog = gtk::Window::builder()
            .transient_for(&parent)
            .modal(true)
            .title(title)
            .default_width(320)
            .resizable(false)
            .child(&content)
            .build();

        // Shared so that Enter, the button and the closing of the window all
        // go through one path; `accept` may only be called once.
        let accept = std::rc::Rc::new(std::cell::RefCell::new(Some(accept)));

        let finish = {
            let dialog = dialog.clone();
            let entry = entry.clone();
            let accept = std::rc::Rc::clone(&accept);
            move || {
                let answer = entry.text().to_string();
                if let Some(accept) = accept.borrow_mut().take()
                    && !answer.trim().is_empty()
                {
                    accept(answer.trim().to_owned());
                }
                dialog.close();
            }
        };

        let on_confirm = finish.clone();
        confirm.connect_clicked(move |_| on_confirm());
        let on_activate = finish;
        entry.connect_activate(move |_| on_activate());

        let closing = dialog.clone();
        cancel.connect_clicked(move |_| closing.close());

        dialog.present();
        entry.grab_focus();
    }

    fn show_about(&self) {
        let detail = concat!(
            "An IRC client with native audio and video conferencing.\n\n",
            "Version ",
            env!("CARGO_PKG_VERSION"),
            "\nGPL-3.0-or-later\n",
            "https://github.com/p145085/kestrel"
        );
        let dialog = gtk::AlertDialog::builder()
            .message("Kestrel")
            .detail(detail)
            .build();
        dialog.show(self.window().as_ref());
    }

    /// Make somewhere for a peer's video and hand the engine the far end.
    ///
    /// This is the one place the two runtimes meet. The sink is created on
    /// this thread because the paintable it produces belongs to GTK and cannot
    /// leave; the sink element itself is ordinary and crosses to the media
    /// thread, where it is plugged into the pipeline. Nothing GTK owns ever
    /// goes the other way.
    fn show_video(&self, peer: &str, target: &str) {
        let Ok(sink) = kestrel_media::gstreamer::ElementFactory::make("gtk4paintablesink").build()
        else {
            self.append(
                SERVER_BUFFER,
                &Line::error(
                    "no gtk4paintablesink, so there is nowhere to draw video; \
                     the call will carry audio only",
                ),
            );
            return;
        };

        // Counted so that "I see a black box" can be told apart from "no
        // video is arriving at all" without anyone having to guess.
        let frames = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        if let Some(pad) = sink.static_pad("sink") {
            let counted = std::sync::Arc::clone(&frames);
            pad.add_probe(
                kestrel_media::gstreamer::PadProbeType::BUFFER,
                move |_, _| {
                    counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    kestrel_media::gstreamer::PadProbeReturn::Ok
                },
            );
        }

        // Checked from the interface's own thread a few seconds later: a probe
        // runs on a streaming thread and must not go near a widget.
        let ui = self.clone();
        let who = peer.to_owned();
        glib::timeout_add_seconds_local_once(5, move || {
            let seen = frames.load(std::sync::atomic::Ordering::Relaxed);
            if seen == 0 {
                ui.append(
                    SERVER_BUFFER,
                    &Line::error(format!(
                        "no video has arrived from {who}; the picture will stay blank"
                    )),
                );
            }
        });

        let paintable: gdk::Paintable = sink.property("paintable");
        let picture = gtk::Picture::builder()
            .paintable(&paintable)
            .content_fit(gtk::ContentFit::Contain)
            .hexpand(true)
            .vexpand(true)
            .build();

        let labelled = gtk::Box::new(gtk::Orientation::Vertical, 2);
        if peer == "you" {
            // Smaller, and first, so it reads as a corner of the call rather
            // than another participant.
            labelled.set_width_request(160);
            labelled.set_hexpand(false);
        }
        labelled.append(&picture);
        labelled.append(
            &gtk::Label::builder()
                .label(peer)
                .css_classes(["dim-label"])
                .build(),
        );

        if peer == "you" {
            self.videos.prepend(&labelled);
        } else {
            self.videos.append(&labelled);
        }
        self.state
            .borrow_mut()
            .pictures
            .push((target.to_owned(), labelled.clone().upcast()));
        self.redraw_video();

        // "you" is not a nickname anybody can have, so it cannot collide
        // with a peer of that name.
        let command = if peer == "you" {
            UiCommand::SelfViewSink { sink }
        } else {
            UiCommand::VideoSink {
                peer: peer.to_owned(),
                sink,
            }
        };
        let _ = self.commands.borrow().send(command);
    }

    /// Take the video away when there is no longer a call.
    fn clear_video(&self) {
        while let Some(child) = self.videos.first_child() {
            self.videos.remove(&child);
        }
        self.state.borrow_mut().pictures.clear();
        self.videos.set_visible(false);
    }

    /// Show only the pictures belonging to the conversation on screen.
    fn redraw_video(&self) {
        let state = self.state.borrow();
        let mut any = false;
        for (target, widget) in &state.pictures {
            let mine = target == &state.current;
            widget.set_visible(mine);
            any |= mine;
        }
        self.videos.set_visible(any);
    }

    /// Choose what this conversation captures from.
    ///
    /// Per conversation rather than per client, because sending a webcam to a
    /// channel and a virtual camera to one person is an ordinary thing to
    /// want and impossible to express with a single global setting.
    fn choose_devices(&self) {
        let Some(parent) = self.window() else { return };
        let target = self.state.borrow().current.clone();
        if target.is_empty() {
            self.append(
                SERVER_BUFFER,
                &Line::error("choose a conversation first; devices are set per conversation"),
            );
            return;
        }

        // Enumerated here rather than cached, because a camera plugged in
        // after the window opened is exactly when somebody goes looking.
        let cameras = kestrel_media::cameras();
        let microphones = kestrel_media::microphones();

        let camera = device_chooser(&cameras);
        let microphone = device_chooser(&microphones);
        let test = gtk::CheckButton::with_label("Use a test picture and tone instead");

        let grid = gtk::Grid::builder()
            .row_spacing(8)
            .column_spacing(12)
            .margin_top(16)
            .margin_bottom(16)
            .margin_start(16)
            .margin_end(16)
            .build();
        grid.attach(
            &gtk::Label::builder()
                .label(format!("What to send in {target}"))
                .xalign(0.0)
                .build(),
            0,
            0,
            2,
            1,
        );
        for (row, (name, chooser)) in [("Camera", &camera), ("Microphone", &microphone)]
            .into_iter()
            .enumerate()
        {
            let row = i32::try_from(row).unwrap_or(0) + 1;
            grid.attach(
                &gtk::Label::builder().label(name).xalign(1.0).build(),
                0,
                row,
                1,
                1,
            );
            grid.attach(chooser, 1, row, 1, 1);
        }
        grid.attach(&test, 1, 3, 1, 1);
        grid.attach(
            &gtk::Label::builder()
                .label(
                    "Applies to the next call here. A call already running keeps what it opened.",
                )
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label"])
                .build(),
            0,
            4,
            2,
            1,
        );

        let cancel = gtk::Button::with_label("Cancel");
        let apply = gtk::Button::with_label("Use these");
        apply.add_css_class("suggested-action");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        buttons.set_halign(gtk::Align::End);
        buttons.append(&cancel);
        buttons.append(&apply);
        grid.attach(&buttons, 0, 5, 2, 1);

        let dialog = gtk::Window::builder()
            .transient_for(&parent)
            .modal(true)
            .title("Devices")
            .child(&grid)
            .build();

        let ui = self.clone();
        let closing = dialog.clone();
        let chosen = (camera.clone(), microphone.clone(), test.clone());
        apply.connect_clicked(move |_| {
            let (camera, microphone, test) = &chosen;
            let _ = ui.commands.borrow().send(UiCommand::Devices {
                target: target.clone(),
                camera: chosen_device(camera),
                microphone: chosen_device(microphone),
                test: test.is_active(),
            });
            closing.close();
        });

        let closing = dialog.clone();
        cancel.connect_clicked(move |_| closing.close());
        dialog.present();
    }

    /// Ask the connection to do something with a call.
    fn call(&self, action: CallAction) {
        let _ = self.commands.borrow().send(UiCommand::Call(action));
    }

    /// Put something through the same path as typing it.
    fn run(&self, input: &str) {
        let buffer = self.state.borrow().current.clone();
        let _ = self.commands.borrow().send(UiCommand::Input {
            buffer,
            text: input.to_owned(),
        });
    }

    /// The window these widgets are in, once it exists.
    ///
    /// Looked up rather than held, because a handler that holds the window
    /// while the window holds the handler is a reference cycle, and GTK's
    /// objects are reference counted.
    fn window(&self) -> Option<gtk::ApplicationWindow> {
        self.view.root().and_downcast::<gtk::ApplicationWindow>()
    }

    /// Ask the connection to leave.
    pub fn quit(&self) {
        let _ = self.commands.borrow().send(UiCommand::Quit {
            reason: "kestrel".to_owned(),
        });
    }
}

/// Open a connection and a window onto it.
///
/// Called once at startup and again for every later connection, so a second
/// server is a second window in the same process rather than a second copy of
/// the program.
pub fn open(
    app: &gtk::Application,
    connect: ConnectConfig,
    session: SessionConfig,
    options: CallOptions,
) -> anyhow::Result<()> {
    let (commands, events) =
        kestrel_ui::connection::start(connect.clone(), session.clone(), options.clone())?;
    let (ui, window) = Window::build(app, commands);
    {
        let mut state = ui.state.borrow_mut();
        state.last = Some((connect, session));
        state.call_options = options;
    }
    pump(ui.clone(), events);

    // Leaving properly rather than dropping the socket, so the server and
    // everyone in the channel see a reason rather than a timeout.
    window.connect_close_request(move |_| {
        ui.quit();
        glib::Propagation::Proceed
    });
    Ok(())
}

impl Window {
    /// Whether this window still has a connection behind it.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.state.borrow().connected
    }

    /// Point this window at a server, in place.
    ///
    /// Reuses the window rather than opening another, because a disconnected
    /// one is of no further use and its scrollback is worth keeping: what was
    /// said before the connection dropped is usually what you want to see
    /// after it comes back.
    pub fn redial(&self, connect: ConnectConfig, session: SessionConfig) -> anyhow::Result<()> {
        self.redial_with(connect, session, self.call_options())
    }

    /// The same, choosing afresh how media is captured.
    pub fn redial_with(
        &self,
        connect: ConnectConfig,
        session: SessionConfig,
        options: CallOptions,
    ) -> anyhow::Result<()> {
        let (commands, events) =
            kestrel_ui::connection::start(connect.clone(), session.clone(), options.clone())?;
        *self.commands.borrow_mut() = commands;
        {
            let mut state = self.state.borrow_mut();
            state.connected = true;
            state.last = Some((connect, session));
            state.call_options = options;
        }

        self.entry.set_sensitive(true);
        self.entry
            .set_placeholder_text(Some("Say something, or /help"));
        self.update_actions();

        // The previous pump ends on its own once the old connection drops its
        // sender, so there is nothing to tear down here.
        pump(self.clone(), events);
        Ok(())
    }

    /// Where this window was last pointed.
    #[must_use]
    pub fn last_connection(&self) -> Option<(ConnectConfig, SessionConfig)> {
        self.state.borrow().last.clone()
    }

    /// How calls from this window are placed.
    #[must_use]
    pub fn call_options(&self) -> CallOptions {
        self.state.borrow().call_options.clone()
    }
}

/// Every action the menu may refer to.
///
/// Named in one place so a menu entry pointing at an action nobody installed
/// cannot slip through: GTK renders such an entry greyed out and says nothing,
/// which looks exactly like a feature that is merely unavailable.
const ACTIONS: [&str; 18] = [
    "devices",
    "next-buffer",
    "previous-buffer",
    "connect",
    "reconnect",
    "verify",
    "reject",
    "join",
    "query",
    "nick",
    "topic",
    "names",
    "part",
    "disconnect",
    "about",
    "call",
    "answer",
    "hangup",
];

/// Where a pointer landed, as a rectangle a popover can point at.
///
/// A pointer is on a whole pixel by the time it reaches a rectangle, so the
/// fraction carries no meaning; clamping keeps a nonsensical coordinate from
/// wrapping into a negative one.
fn at(value: f64) -> i32 {
    let clamped = value.round().clamp(0.0, f64::from(i32::MAX));
    #[allow(clippy::cast_possible_truncation)]
    {
        clamped as i32
    }
}

/// What a right click on a buffer in the list offers.
///
/// A server buffer is the connection itself, so the only thing to do with it
/// is disconnect; everything else is a conversation that can simply be closed.
fn buffer_menu(id: &str) -> gio::Menu {
    let menu = gio::Menu::new();
    if id.is_empty() {
        menu.append(Some("Disconnect"), Some("win.disconnect"));
        menu.append(Some("Reconnect"), Some("win.reconnect"));
    } else {
        menu.append(Some("Close"), Some(&format!("win.close-buffer::{id}")));
    }
    menu
}

/// What a right click on somebody in the member list offers.
///
/// Built per person rather than shared, because the actions have to carry who
/// they are about and a menu model has no other way to say so.
fn member_menu(nick: &str) -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append(
        Some(&format!("Call {nick}")),
        Some(&format!("win.call-nick::{nick}")),
    );
    menu.append(
        Some(&format!("Message {nick}")),
        Some(&format!("win.query-nick::{nick}")),
    );
    menu.append(
        Some(&format!("Who is {nick}")),
        Some(&format!("win.whois-nick::{nick}")),
    );
    menu
}

/// A dropdown over a list of device names, with the default first.
fn device_chooser(names: &[String]) -> gtk::DropDown {
    let mut entries: Vec<&str> = vec!["(system default)"];
    entries.extend(names.iter().map(String::as_str));
    let chooser = gtk::DropDown::from_strings(&entries);
    chooser.set_hexpand(true);
    chooser
}

/// What a chooser is pointing at, or `None` for the default.
fn chosen_device(chooser: &gtk::DropDown) -> Option<String> {
    let selected = chooser.selected();
    if selected == 0 {
        return None;
    }
    chooser
        .model()
        .and_downcast::<gtk::StringList>()
        .and_then(|list| list.string(selected))
        .map(|name| name.to_string())
}

/// The menu bar's contents.
///
/// A menu rather than only slash commands: the commands are faster once known,
/// but nothing in a text box tells a new user that any of this exists.
fn menu_model() -> gio::Menu {
    let server = gio::Menu::new();
    server.append(Some("New Connection…"), Some("win.connect"));
    server.append(Some("Reconnect"), Some("win.reconnect"));
    server.append(Some("Join Channel…"), Some("win.join"));
    server.append(Some("Open Conversation…"), Some("win.query"));
    server.append(Some("Change Nickname…"), Some("win.nick"));
    server.append(Some("Disconnect"), Some("win.disconnect"));

    let channel = gio::Menu::new();
    channel.append(Some("Set Topic…"), Some("win.topic"));
    channel.append(Some("Refresh Members"), Some("win.names"));
    channel.append(Some("Leave Channel"), Some("win.part"));

    let call = gio::Menu::new();
    call.append(Some("Start Call…"), Some("win.call"));
    call.append(Some("Devices…"), Some("win.devices"));
    call.append(Some("Answer"), Some("win.answer"));
    call.append(Some("Reject"), Some("win.reject"));
    call.append(Some("Confirm Spoken Phrase"), Some("win.verify"));
    call.append(Some("Hang Up"), Some("win.hangup"));

    let buffers = gio::Menu::new();
    buffers.append(Some("Next"), Some("win.next-buffer"));
    buffers.append(Some("Previous"), Some("win.previous-buffer"));

    let help = gio::Menu::new();
    help.append(Some("About Kestrel"), Some("win.about"));

    let bar = gio::Menu::new();
    bar.append_submenu(Some("Server"), &server);
    bar.append_submenu(Some("Channel"), &channel);
    bar.append_submenu(Some("Call"), &call);
    bar.append_submenu(Some("Buffers"), &buffers);
    bar.append_submenu(Some("Help"), &help);
    bar
}

/// Which tag paints the name at the start of a line.
fn nick_tag(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Highlight => "highlight",
        LineKind::Action => "action",
        LineKind::Notice => "notice",
        LineKind::Own => "own-nick",
        _ => "nick",
    }
}

/// Which tag paints the rest of it.
fn body_tag(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Highlight => "highlight",
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
    tag("highlight", "#7dcfff", true);
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

#[cfg(test)]
mod tests {
    use super::{ACTIONS, menu_model};
    use gtk::gio;
    use gtk::prelude::*;

    /// Every action name the menu refers to, submenus included.
    fn referenced(model: &gio::MenuModel, into: &mut Vec<String>) {
        for index in 0..model.n_items() {
            if let Some(action) = model
                .item_attribute_value(index, "action", None)
                .and_then(|value| value.str().map(str::to_owned))
            {
                into.push(action);
            }
            if let Some(submenu) = model.item_link(index, "submenu") {
                referenced(&submenu, into);
            }
        }
    }

    #[test]
    fn every_menu_entry_points_at_an_action_that_exists() {
        let mut found = Vec::new();
        referenced(menu_model().upcast_ref(), &mut found);

        assert!(!found.is_empty(), "the menu model produced nothing");
        for action in &found {
            let name = action
                .strip_prefix("win.")
                .unwrap_or_else(|| panic!("{action} is not a window action"));
            assert!(
                ACTIONS.contains(&name),
                "the menu refers to {name}, which is never installed"
            );
        }
    }

    #[test]
    fn every_action_is_reachable_from_the_menu() {
        // The other direction: an action nobody can invoke is dead code that
        // looks like a feature.
        let mut found = Vec::new();
        referenced(menu_model().upcast_ref(), &mut found);

        for name in ACTIONS {
            assert!(
                found.iter().any(|action| action == &format!("win.{name}")),
                "{name} is installed but appears in no menu"
            );
        }
    }
}
