//! Turning session events into lines on a terminal.

use kestrel_net::ClientEvent;
use kestrel_session::{Ended, Event, MessageKind, Target};

use crate::ui::{State, colour, status, warn};

/// Show one event.
pub fn show(event: &ClientEvent, state: &mut State) {
    match event {
        ClientEvent::Connected => status("connected; registering"),
        ClientEvent::Session(event) => show_session(event, state),
        // Raw traffic is available for debugging but not printed by default:
        // it would bury the conversation it is meant to explain.
        ClientEvent::RawIn(_) | ClientEvent::Disconnected(_) => {}
    }
}

#[allow(clippy::too_many_lines)]
fn show_session(event: &Event, state: &mut State) {
    match event {
        Event::Registered { nick } => {
            state.nick = text(nick);
            state.registered = true;
            status(&format!("registered as {}", state.nick));
        }
        Event::CapsEnabled(caps) => {
            state.server_echoes = caps.iter().any(|c| c == "echo-message");
            if !caps.is_empty() {
                status(&format!("capabilities: {}", caps.join(" ")));
            }
        }
        Event::Names { channel, members } => {
            let names: Vec<String> = members.iter().map(|m| text(m)).collect();
            event_line(
                channel,
                &format!("{} members: {}", names.len(), names.join(" ")),
            );
        }
        Event::LoggedIn { account } => {
            status(&format!("logged in as {}", text(account)));
        }
        Event::LoginFailed { reason } => warn(&format!("login failed: {reason}")),

        Event::Message {
            target,
            from,
            text: body,
            kind,
            ..
        } => show_message(target, &text(&from.nick), body, *kind, state),

        Event::Joined {
            channel,
            who,
            is_self,
        } => {
            if *is_self {
                status(&format!("joined {}", text(channel)));
                state.target = Some(text(channel));
            } else {
                event_line(channel, &format!("{} joined", text(&who.nick)));
            }
        }
        Event::Parted {
            channel,
            who,
            reason,
            is_self,
        } => {
            let why = reason
                .as_ref()
                .map(|r| format!(" ({})", text(r)))
                .unwrap_or_default();
            if *is_self {
                status(&format!("left {}{why}", text(channel)));
            } else {
                event_line(channel, &format!("{} left{why}", text(&who.nick)));
            }
        }
        Event::Quit {
            who,
            reason,
            channels,
        } => {
            let why = reason
                .as_ref()
                .map(|r| format!(" ({})", text(r)))
                .unwrap_or_default();
            // Name the channels, so it is clear where the person went from.
            let where_ = if channels.is_empty() {
                String::new()
            } else {
                let names: Vec<String> = channels.iter().map(|c| text(c)).collect();
                format!(" [{}]", names.join(" "))
            };
            println!(
                "{}--{} {} quit{why}{where_}",
                colour::DIM,
                colour::RESET,
                text(&who.nick)
            );
        }
        Event::Kicked {
            channel,
            who,
            by,
            reason,
            is_self,
        } => {
            let why = reason
                .as_ref()
                .map(|r| format!(" ({})", text(r)))
                .unwrap_or_default();
            let line = format!("{} was kicked by {}{why}", text(who), text(&by.nick));
            if *is_self {
                warn(&format!("you were kicked from {}{why}", text(channel)));
            } else {
                event_line(channel, &line);
            }
        }
        Event::NickChanged {
            old, new, is_self, ..
        } => {
            if *is_self {
                state.nick = text(new);
                status(&format!("you are now {}", state.nick));
            } else {
                status(&format!("{} is now {}", text(old), text(new)));
            }
        }
        Event::Topic {
            channel,
            topic,
            setter,
        } => match topic {
            Some(topic) => {
                let by = setter
                    .as_ref()
                    .map(|s| format!(" (set by {})", text(s)))
                    .unwrap_or_default();
                event_line(channel, &format!("topic: {}{by}", text(topic)));
            }
            None => event_line(channel, "no topic set"),
        },
        Event::AwayChanged { nick, message } => match message {
            Some(message) => status(&format!("{} is away: {}", text(nick), text(message))),
            None => status(&format!("{} is back", text(nick))),
        },
        Event::Invited { channel, by } => {
            status(&format!(
                "{} invited you to {}",
                text(&by.nick),
                text(channel)
            ));
        }
        Event::ModeChanged {
            target,
            spec,
            params,
            by,
        } => {
            let extra = params.iter().map(|p| text(p)).collect::<Vec<_>>().join(" ");
            event_line(
                target,
                &format!("mode {} {extra} by {}", text(spec), text(&by.nick)),
            );
        }
        Event::StandardReply {
            severity,
            command,
            code,
            text,
        } => {
            let detail = if code.is_empty() {
                String::new()
            } else {
                format!(" [{}]", text_of(code))
            };
            let line = format!("{}{detail}: {}", text_of(command), text_of(text));
            if severity == b"FAIL" {
                warn(&line);
            } else {
                status(&line);
            }
        }
        Event::Numeric { code, params, .. } => show_numeric(*code, params),
        Event::Ended(Ended::ServerError(why)) => warn(&format!("server closed: {why}")),
        Event::Ended(Ended::RegistrationFailed(why)) => warn(&format!("could not register: {why}")),

        // Roster churn and tag-only traffic are tracked but not printed: a
        // line for every typing notification would drown the conversation.
        // CALL messages are acted on before they reach here.
        Event::RosterChanged { .. }
        | Event::TagMessage { .. }
        | Event::Call { .. }
        | Event::Raw(_) => {}
    }
}

fn show_message(target: &Target, from: &str, body: &[u8], kind: MessageKind, state: &State) {
    let where_ = match target {
        // A message addressed to us belongs to the sender's window, not ours.
        Target::Channel(name) => text(name),
        Target::Direct(_) => from.to_owned(),
    };
    let body = text(body);

    // CTCP ACTION is what `/me` produces; showing the control bytes raw would
    // be noise.
    if let Some(action) = body
        .strip_prefix('\u{1}')
        .and_then(|b| b.strip_suffix('\u{1}'))
        .and_then(|b| b.strip_prefix("ACTION "))
    {
        println!(
            "{}{where_}{} {}* {from} {action}{}",
            colour::DIM,
            colour::RESET,
            colour::CYAN,
            colour::RESET
        );
        return;
    }

    let highlighted = !state.nick.is_empty() && body.contains(&state.nick);
    let (open, close) = match (kind, highlighted) {
        (MessageKind::Notice, _) => (colour::YELLOW, colour::RESET),
        (MessageKind::Privmsg, true) => (colour::GREEN, colour::RESET),
        (MessageKind::Privmsg, false) => (colour::BOLD, colour::RESET),
    };
    println!(
        "{}{where_}{} <{open}{from}{close}> {body}",
        colour::DIM,
        colour::RESET
    );
}

fn show_numeric(code: u16, params: &[Vec<u8>]) {
    use kestrel_proto::numeric as n;

    match code {
        // The welcome burst and MOTD read best as plain text.
        n::RPL_WELCOME
        | n::RPL_YOURHOST
        | n::RPL_CREATED
        | n::RPL_MOTD
        | n::RPL_MOTDSTART
        | n::RPL_ENDOFMOTD
        | n::RPL_LUSERCLIENT
        | n::RPL_LUSERME => {
            if let Some(last) = params.last() {
                println!("{}{}{}", colour::DIM, text(last), colour::RESET);
            }
        }
        // Listings are worth showing in full.
        n::RPL_NAMREPLY
        | n::RPL_WHOISUSER
        | n::RPL_WHOISCHANNELS
        | n::RPL_WHOISSERVER
        | n::RPL_LIST
        | n::RPL_TOPIC
        | n::RPL_BANLIST
        | n::RPL_WHOREPLY => {
            println!("{}", join(params));
        }
        // Everything from 400 up is an error the user asked for.
        400..=599 => warn(&join(params)),
        // Anything else is noise unless someone asked for it.
        _ => {}
    }
}

/// Render bytes for a message, named so it does not shadow `text`.
fn text_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn event_line(channel: &[u8], text: &str) {
    println!(
        "{}{}{} {}-- {text}{}",
        colour::DIM,
        crate::render::text(channel),
        colour::RESET,
        colour::BLUE,
        colour::RESET
    );
}

fn join(params: &[Vec<u8>]) -> String {
    params.iter().map(|p| text(p)).collect::<Vec<_>>().join(" ")
}

/// Render bytes for a terminal.
///
/// Lossy on purpose: IRC has no guaranteed encoding, and refusing to print a
/// message because one byte is not UTF-8 is worse than printing a replacement
/// character.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::text;

    #[test]
    fn invalid_utf8_is_shown_rather_than_dropped() {
        assert_eq!(text(b"caf\xe9"), "caf\u{fffd}");
    }

    #[test]
    fn ordinary_text_is_unchanged() {
        assert_eq!(text(b"hello"), "hello");
    }
}
