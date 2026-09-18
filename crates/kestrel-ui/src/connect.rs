//! Asking where to connect.
//!
//! A client that can only be pointed at a server from the command line is a
//! development tool. This is the window that makes it an application, and the
//! one that lets a second connection be opened without starting the program
//! again.

use gtk::prelude::*;
use kestrel_net::ConnectConfig;
use kestrel_session::SessionConfig;
use kestrel_session::config::Sasl;

use kestrel_ui::connection::CallOptions;

use crate::window;

/// Show the connection window.
// Building a form is linear by nature: every field is three lines and
// splitting them across functions would only hide the layout.
#[allow(clippy::too_many_lines)]
pub fn show(app: &gtk::Application, from: Option<window::Window>, options: &CallOptions) {
    let server = entry("127.0.0.1");
    let port = entry("6667");
    let nick = entry(&default_nick());
    let channels = entry("#test");
    let account = entry("");
    let password = entry("");
    password.set_visibility(false);
    password.set_input_purpose(gtk::InputPurpose::Password);

    // A camera opens once. Two clients on one machine cannot both have it,
    // and the second one fails in a way that reads like a broken call rather
    // than a busy device -- so there is a way to say "do not even try".
    let test_media = gtk::CheckButton::with_label("Use a test picture instead of a camera");
    let tls = gtk::CheckButton::with_label("Connect with TLS");
    let insecure = gtk::CheckButton::with_label("Accept any certificate");
    insecure.set_sensitive(false);
    insecure.set_tooltip_text(Some("Only meaningful with TLS"));

    // The default port differs between plain and TLS, and a user who ticks the
    // box and then cannot connect to 6667 has been given a puzzle rather than a
    // client. Changed only while the field still holds the other default, so a
    // port typed on purpose is never overwritten.
    let ports = (port.clone(), tls.clone());
    tls.connect_toggled(move |tls| {
        let (port, _) = &ports;
        let secure = tls.is_active();
        let text = port.text();
        if secure && text == "6667" {
            port.set_text("6697");
        } else if !secure && text == "6697" {
            port.set_text("6667");
        }
    });

    let secure = tls.clone();
    let dependent = insecure.clone();
    secure.connect_toggled(move |tls| {
        dependent.set_sensitive(tls.is_active());
        if !tls.is_active() {
            dependent.set_active(false);
        }
    });

    let grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();
    for (row, (label, field)) in [
        ("Server", &server),
        ("Port", &port),
        ("Nickname", &nick),
        ("Channels", &channels),
        ("Account", &account),
        ("Password", &password),
    ]
    .into_iter()
    .enumerate()
    {
        let row = i32::try_from(row).unwrap_or(0);
        grid.attach(
            &gtk::Label::builder().label(label).xalign(1.0).build(),
            0,
            row,
            1,
            1,
        );
        grid.attach(field, 1, row, 1, 1);
    }
    // Said plainly, because an unauthenticated call cannot pin a key to
    // anybody: the phrase still proves nobody is in the middle right now, but
    // nothing carries over to the next call.
    grid.attach(
        &gtk::Label::builder()
            .label("Leave empty to connect without an account.\nCalls can then be verified, but not remembered.")
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build(),
        1,
        6,
        1,
        1,
    );
    grid.attach(&test_media, 1, 7, 1, 1);
    grid.attach(&tls, 1, 8, 1, 1);
    grid.attach(&insecure, 1, 9, 1, 1);

    let problem = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .build();
    problem.add_css_class("error");
    grid.attach(&problem, 0, 10, 2, 1);

    let cancel = gtk::Button::with_label("Cancel");
    let connect = gtk::Button::with_label("Connect");
    connect.add_css_class("suggested-action");

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&connect);
    grid.attach(&buttons, 0, 11, 2, 1);

    let dialog = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Connect to a server")
        .resizable(false)
        .child(&grid)
        .build();

    let go = {
        let app = app.clone();
        let dialog = dialog.clone();
        // A window whose connection has ended is reused rather than left
        // behind as a dead one beside a live one.
        let reuse = from.filter(|window| !window.is_connected());
        let options = options.clone();

        let fields = Fields {
            server: server.clone(),
            port: port.clone(),
            nick: nick.clone(),
            channels: channels.clone(),
            account: account.clone(),
            password: password.clone(),
            tls: tls.clone(),
            insecure: insecure.clone(),
            test_media: test_media.clone(),
        };
        let problem = problem.clone();
        move || match fields.read() {
            Ok((connect, session)) => {
                let mut chosen = options.clone();
                chosen.test_media |= fields.test_media.is_active();
                let opened = match &reuse {
                    Some(window) => window.redial_with(connect, session, chosen),
                    None => window::open(&app, connect, session, chosen),
                };
                if let Err(error) = opened {
                    problem.set_text(&format!("{error:#}"));
                    problem.set_visible(true);
                    return;
                }
                dialog.close();
            }
            Err(why) => {
                problem.set_text(&why);
                problem.set_visible(true);
            }
        }
    };

    let clicked = go.clone();
    connect.connect_clicked(move |_| clicked());
    // Enter anywhere in the form connects, which is what every other client does.
    for field in [&server, &port, &nick, &channels, &account, &password] {
        let activated = go.clone();
        field.connect_activate(move |_| activated());
    }

    let closing = dialog.clone();
    cancel.connect_clicked(move |_| closing.close());

    dialog.present();
    nick.grab_focus();
}

/// The fields, so reading them is one operation rather than six captures.
#[derive(Clone)]
struct Fields {
    server: gtk::Entry,
    port: gtk::Entry,
    nick: gtk::Entry,
    channels: gtk::Entry,
    account: gtk::Entry,
    password: gtk::Entry,
    tls: gtk::CheckButton,
    insecure: gtk::CheckButton,
    test_media: gtk::CheckButton,
}

impl Fields {
    /// Turn what was typed into what the connection needs.
    ///
    /// Returns the reason rather than an error type: everything that can go
    /// wrong here is something the person at the keyboard can fix, and the
    /// message goes straight back into the form.
    fn read(&self) -> Result<(ConnectConfig, SessionConfig), String> {
        let host = self.server.text().trim().to_owned();
        if host.is_empty() {
            return Err("A server is needed.".to_owned());
        }

        let port: u16 = self
            .port
            .text()
            .trim()
            .parse()
            .map_err(|_| "The port has to be a number between 1 and 65535.".to_owned())?;

        let nick = self.nick.text().trim().to_owned();
        if nick.is_empty() {
            return Err("A nickname is needed.".to_owned());
        }
        // Caught here rather than by the server, which would answer with a
        // numeric and leave the user looking at a client that did nothing.
        if nick.contains(' ') {
            return Err("A nickname cannot contain a space.".to_owned());
        }

        let tls = self.tls.is_active();
        let mut connect = if tls {
            ConnectConfig::tls(host, port)
        } else {
            ConnectConfig::plain(host, port)
        };
        connect.danger_accept_invalid_certs = tls && self.insecure.is_active();

        let mut session = SessionConfig::new(nick);
        session.autojoin = self
            .channels
            .text()
            .split(',')
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .map(String::into_bytes)
            .collect();

        let account = self.account.text().trim().to_owned();
        if !account.is_empty() {
            let password = self.password.text().to_string();
            if password.is_empty() {
                return Err("An account needs a password.".to_owned());
            }
            session.sasl = Some(Sasl::Plain {
                account: account.into_bytes(),
                password: password.into_bytes(),
            });
        }

        Ok((connect, session))
    }
}

fn entry(initial: &str) -> gtk::Entry {
    gtk::Entry::builder()
        .text(initial)
        .hexpand(true)
        .width_request(220)
        .build()
}

/// A nickname worth suggesting.
///
/// The account name is a better first guess than a fixed default, since it is
/// usually what a person already calls themselves.
fn default_nick() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .map(|name| {
            name.chars()
                .filter(|c| c.is_ascii_alphanumeric() || "-_[]{}\\`|".contains(*c))
                .collect::<String>()
        })
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "kestrel".to_owned())
}
