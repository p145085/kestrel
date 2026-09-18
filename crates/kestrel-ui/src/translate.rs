//! Turning what the session reports into what the interface shows.
//!
//! Kept apart from both the connection and the widgets on purpose: deciding
//! which buffer a line belongs in is the part most worth testing, and it needs
//! neither a socket nor a display to exercise.

use std::collections::HashMap;

use kestrel_session::event::{Ended, Event, MessageKind, Sender, Target};

use crate::event::{AppEvent, BufferId, Line, SERVER_BUFFER};

/// Decides where things go.
///
/// Holds our own nickname because routing depends on it: a private message
/// belongs in the conversation with the *other* party, and which party that is
/// depends on whether we sent it or received it.
#[derive(Debug, Default)]
pub struct Translator {
    me: String,
    /// Who is in each channel, as last known.
    ///
    /// Kept here rather than asked for, because the server announces every
    /// change and only sends a full list when one is requested. A member list
    /// that is only refreshed by `NAMES` goes stale the moment anyone joins,
    /// leaves or changes nickname.
    rosters: HashMap<BufferId, Vec<String>>,
}

/// Strip the membership prefixes a name may be listed with.
///
/// The roster stores names as the server presents them, `@alice` and all, so
/// matching one against a bare nickname has to ignore the prefix -- while
/// changing an entry has to keep it.
fn without_prefix(name: &str) -> &str {
    name.trim_start_matches(['~', '&', '@', '%', '+'])
}

impl Translator {
    /// A translator that does not yet know who we are.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Our nickname, as far as the server is concerned.
    #[must_use]
    pub fn me(&self) -> &str {
        &self.me
    }

    /// Translate one event into everything the interface should do about it.
    // One arm per event, for the same reason as `command::parse`.
    #[allow(clippy::too_many_lines)]
    pub fn translate(&mut self, event: Event) -> Vec<AppEvent> {
        match event {
            Event::Registered { nick } => {
                self.me = text(&nick);
                vec![
                    AppEvent::Registered {
                        nick: self.me.clone(),
                    },
                    server_line(Line::status(format!("registered as {}", self.me))),
                ]
            }
            Event::CapsEnabled(caps) => {
                if caps.is_empty() {
                    Vec::new()
                } else {
                    vec![server_line(Line::status(format!(
                        "capabilities: {}",
                        caps.join(" ")
                    )))]
                }
            }
            Event::LoggedIn { account } => vec![server_line(Line::status(format!(
                "logged in as {}",
                text(&account)
            )))],
            Event::LoginFailed { reason } => {
                vec![server_line(Line::error(format!("login failed: {reason}")))]
            }
            Event::Message {
                target,
                from,
                text: body,
                kind,
                ..
            } => self.message(&target, &from, &body, kind),
            Event::Joined {
                channel,
                who,
                is_self,
            } => {
                let buffer = text(&channel);
                let nick = text(&who.nick);
                if is_self {
                    // Opened before the join is announced, so the line lands in
                    // a buffer that already exists and the user is looking at.
                    vec![
                        AppEvent::OpenBuffer {
                            buffer: buffer.clone(),
                        },
                        line(
                            buffer,
                            Line::status(format!("you joined {}", text(&channel))),
                        ),
                    ]
                } else {
                    let mut out =
                        vec![line(buffer.clone(), Line::status(format!("{nick} joined")))];
                    self.rosters.entry(buffer.clone()).or_default().push(nick);
                    out.extend(self.roster_of(&buffer));
                    out
                }
            }
            Event::Parted {
                channel,
                who,
                reason,
                is_self,
            } => {
                let buffer = text(&channel);
                if is_self {
                    self.rosters.remove(&buffer);
                    return vec![AppEvent::CloseBuffer { buffer }];
                }
                let nick = text(&who.nick);
                let why = reason
                    .map(|r| format!(" ({})", text(&r)))
                    .unwrap_or_default();
                let mut out = vec![line(
                    buffer.clone(),
                    Line::status(format!("{nick} left{why}")),
                )];
                self.remove_member(&buffer, &nick);
                out.extend(self.roster_of(&buffer));
                out
            }
            Event::Quit {
                who,
                reason,
                channels,
            } => {
                let nick = text(&who.nick);
                let why = reason
                    .map(|r| format!(" ({})", text(&r)))
                    .unwrap_or_default();
                let mut out = Vec::new();
                for channel in channels {
                    let buffer = text(&channel);
                    out.push(line(
                        buffer.clone(),
                        Line::status(format!("{nick} disconnected{why}")),
                    ));
                    self.remove_member(&buffer, &nick);
                    out.extend(self.roster_of(&buffer));
                }
                out
            }
            Event::Kicked {
                channel,
                who,
                by,
                reason,
                is_self,
            } => {
                let buffer = text(&channel);
                let target = text(&who);
                let actor = text(&by.nick);
                let why = reason
                    .map(|r| format!(" ({})", text(&r)))
                    .unwrap_or_default();
                let said = if is_self {
                    format!("{actor} removed you from the channel{why}")
                } else {
                    format!("{actor} removed {target}{why}")
                };
                let mut out = vec![line(buffer.clone(), Line::status(said))];
                if is_self {
                    // Kept open, unlike a part: the user did not choose this,
                    // and closing the buffer would take the reason with it.
                    self.rosters.insert(buffer.clone(), Vec::new());
                } else {
                    self.remove_member(&buffer, &target);
                }
                out.extend(self.roster_of(&buffer));
                out
            }
            Event::NickChanged {
                old,
                new,
                channels,
                is_self,
            } => {
                let old = text(&old);
                let new = text(&new);
                let said = if is_self {
                    format!("you are now {new}")
                } else {
                    format!("{old} is now {new}")
                };

                let mut out: Vec<AppEvent> = Vec::new();
                for channel in channels {
                    let buffer = text(&channel);
                    out.push(line(buffer.clone(), Line::status(said.clone())));
                    self.rename_member(&buffer, &old, &new);
                    out.extend(self.roster_of(&buffer));
                }
                if is_self {
                    self.me.clone_from(&new);
                    out.push(AppEvent::NickChanged { nick: new });
                    if out.len() == 1 {
                        // Not in any channel yet, so it would otherwise pass
                        // without a word anywhere.
                        out.push(server_line(Line::status(said)));
                    }
                }
                out
            }
            Event::Topic {
                channel,
                topic,
                setter,
            } => {
                let buffer = text(&channel);
                let topic = topic.map(|t| text(&t)).unwrap_or_default();
                let said = if topic.is_empty() {
                    "no topic set".to_owned()
                } else {
                    match setter {
                        Some(who) => format!("topic (set by {}): {topic}", text(&who)),
                        None => format!("topic: {topic}"),
                    }
                };
                vec![
                    AppEvent::Topic {
                        buffer: buffer.clone(),
                        topic,
                    },
                    line(buffer, Line::status(said)),
                ]
            }
            Event::Names { channel, members } => {
                let buffer = text(&channel);
                let members: Vec<String> = members.iter().map(|m| text(m)).collect();
                self.rosters.insert(buffer.clone(), members.clone());
                vec![AppEvent::Roster { buffer, members }]
            }
            Event::ModeChanged {
                target,
                spec,
                params,
                by,
            } => {
                let mut said = format!("{} set {}", text(&by.nick), text(&spec));
                for param in &params {
                    said.push(' ');
                    said.push_str(&text(param));
                }
                vec![line(text(&target), Line::status(said))]
            }
            Event::AwayChanged { nick, message } => {
                let nick = text(&nick);
                let said = match message {
                    Some(why) => format!("{nick} is away ({})", text(&why)),
                    None => format!("{nick} is back"),
                };
                vec![server_line(Line::status(said))]
            }
            Event::Invited { channel, by } => vec![server_line(Line::status(format!(
                "{} invited you to {}",
                text(&by.nick),
                text(&channel)
            )))],
            Event::StandardReply {
                severity,
                command,
                text: body,
                ..
            } => {
                let said = format!("{} {}: {}", text(&severity), text(&command), text(&body));
                let reply = if severity.eq_ignore_ascii_case(b"FAIL") {
                    Line::error(said)
                } else {
                    Line::status(said)
                };
                vec![server_line(reply)]
            }
            Event::Numeric {
                code,
                params,
                text: body,
            } => {
                // The trailing text is also the last parameter, so showing
                // both means dropping it from the parameters first.
                let fixed = if body.is_some() {
                    params.split_last().map_or(&[][..], |(_, rest)| rest)
                } else {
                    &params[..]
                };

                let mut said = String::new();
                for param in fixed {
                    said.push_str(&text(param));
                    said.push(' ');
                }
                if let Some(body) = body {
                    said.push_str(&text(&body));
                }
                let said = said.trim().to_owned();
                if said.is_empty() {
                    return Vec::new();
                }
                // Numerics in the 400s and 500s are refusals; the rest is
                // informational output such as WHOIS.
                let reply = if (400..600).contains(&code) {
                    Line::error(said)
                } else {
                    Line::status(said)
                };
                vec![server_line(reply)]
            }
            Event::Ended(Ended::ServerError(why)) => vec![AppEvent::Disconnected {
                reason: format!("server error: {why}"),
            }],
            Event::Ended(Ended::RegistrationFailed(why)) => vec![AppEvent::Disconnected {
                reason: format!("could not register: {why}"),
            }],
            // Calls are the call machinery's business, typing notifications
            // are not shown yet, and an unrecognised message is not worth
            // putting in front of anybody.
            Event::Call { .. }
            | Event::TagMessage { .. }
            | Event::RosterChanged { .. }
            | Event::Raw(_) => Vec::new(),
        }
    }

    /// The current membership of a channel, as an event.
    fn roster_of(&self, buffer: &str) -> Option<AppEvent> {
        self.rosters.get(buffer).map(|members| AppEvent::Roster {
            buffer: buffer.to_owned(),
            members: members.clone(),
        })
    }

    fn remove_member(&mut self, buffer: &str, nick: &str) {
        if let Some(members) = self.rosters.get_mut(buffer) {
            members.retain(|member| without_prefix(member) != nick);
        }
    }

    /// Rename somebody, keeping whatever prefix they held.
    fn rename_member(&mut self, buffer: &str, old: &str, new: &str) {
        let Some(members) = self.rosters.get_mut(buffer) else {
            return;
        };
        for member in members.iter_mut() {
            if without_prefix(member) == old {
                let prefix_len = member.len() - without_prefix(member).len();
                let mut renamed = member[..prefix_len].to_owned();
                renamed.push_str(new);
                *member = renamed;
            }
        }
    }

    /// Route one message to the buffer it belongs in.
    fn message(
        &self,
        target: &Target,
        from: &Sender,
        body: &[u8],
        kind: MessageKind,
    ) -> Vec<AppEvent> {
        let nick = text(&from.nick);
        let mine = nick == self.me;

        let buffer: BufferId = if target.is_channel() {
            text(target.name())
        } else if mine {
            // Our own message, echoed back by the server. The conversation is
            // with whoever it was addressed to, not with ourselves.
            text(target.name())
        } else {
            nick.clone()
        };

        // A server notice has no conversation to belong to.
        let buffer = if from.is_server && !target.is_channel() {
            SERVER_BUFFER.to_owned()
        } else {
            buffer
        };

        let body = text(body);
        let rendered = match action_text(&body) {
            Some(what) => Line::action(nick, what),
            None if kind == MessageKind::Notice => Line::notice(nick, body),
            None if mine => Line::own(nick, body),
            None => Line::message(nick, body),
        };
        vec![line(buffer, rendered)]
    }
}

/// The text of a CTCP action, if that is what this is.
///
/// `\x01ACTION waves\x01` is how "/me waves" travels, and showing the control
/// characters instead of the intent is a mark of a client that has not read
/// its own traffic.
fn action_text(body: &str) -> Option<&str> {
    let inner = body.strip_prefix('\u{1}')?;
    let inner = inner.strip_suffix('\u{1}').unwrap_or(inner);
    inner.strip_prefix("ACTION ")
}

/// IRC carries bytes, not text, so anything undecodable is shown rather than
/// dropped: a mangled line the user can see beats a line that never arrives.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn line(buffer: BufferId, line: Line) -> AppEvent {
    AppEvent::Line { buffer, line }
}

fn server_line(line: Line) -> AppEvent {
    AppEvent::Line {
        buffer: SERVER_BUFFER.to_owned(),
        line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::LineKind;

    fn sender(nick: &str) -> Sender {
        Sender {
            nick: nick.as_bytes().to_vec(),
            mask: None,
            account: None,
            is_server: false,
        }
    }

    fn registered(nick: &str) -> Translator {
        let mut translator = Translator::new();
        translator.translate(Event::Registered {
            nick: nick.as_bytes().to_vec(),
        });
        translator
    }

    fn only_line(events: Vec<AppEvent>) -> (BufferId, Line) {
        events
            .into_iter()
            .find_map(|event| match event {
                AppEvent::Line { buffer, line } => Some((buffer, line)),
                _ => None,
            })
            .expect("expected a line")
    }

    #[test]
    fn a_channel_message_lands_in_the_channel() {
        let mut translator = registered("me");
        let (buffer, line) = only_line(translator.translate(Event::Message {
            target: Target::Channel(b"#rust".to_vec()),
            from: sender("alice"),
            text: b"hello".to_vec(),
            kind: MessageKind::Privmsg,
            time: None,
        }));

        assert_eq!(buffer, "#rust");
        assert_eq!(line.who.as_deref(), Some("alice"));
        assert_eq!(line.text, "hello");
    }

    #[test]
    fn a_private_message_lands_in_the_senders_buffer() {
        let mut translator = registered("me");
        let (buffer, _) = only_line(translator.translate(Event::Message {
            target: Target::Direct(b"me".to_vec()),
            from: sender("alice"),
            text: b"psst".to_vec(),
            kind: MessageKind::Privmsg,
            time: None,
        }));

        // Not "me": the conversation is with alice, and filing it under our own
        // nickname would give every correspondent the same buffer.
        assert_eq!(buffer, "alice");
    }

    #[test]
    fn our_own_private_message_lands_in_the_recipients_buffer() {
        let mut translator = registered("me");
        // What `echo-message` sends back: from us, addressed to them.
        let (buffer, line) = only_line(translator.translate(Event::Message {
            target: Target::Direct(b"alice".to_vec()),
            from: sender("me"),
            text: b"hi".to_vec(),
            kind: MessageKind::Privmsg,
            time: None,
        }));

        assert_eq!(buffer, "alice", "our echo belongs in their conversation");
        assert_eq!(line.kind, LineKind::Own);
    }

    #[test]
    fn an_action_is_shown_as_one() {
        let mut translator = registered("me");
        let (_, line) = only_line(translator.translate(Event::Message {
            target: Target::Channel(b"#rust".to_vec()),
            from: sender("alice"),
            text: b"\x01ACTION waves\x01".to_vec(),
            kind: MessageKind::Privmsg,
            time: None,
        }));

        assert_eq!(line.kind, LineKind::Action);
        assert_eq!(line.text, "waves", "the control characters are not content");
    }

    #[test]
    fn joining_opens_the_buffer_before_anything_is_written_to_it() {
        let mut translator = registered("me");
        let events = translator.translate(Event::Joined {
            channel: b"#rust".to_vec(),
            who: sender("me"),
            is_self: true,
        });

        match events.first() {
            Some(AppEvent::OpenBuffer { buffer }) => assert_eq!(buffer, "#rust"),
            other => panic!("the buffer must be opened first, got {other:?}"),
        }
    }

    #[test]
    fn our_own_nick_change_is_remembered() {
        let mut translator = registered("me");
        translator.translate(Event::NickChanged {
            old: b"me".to_vec(),
            new: b"myself".to_vec(),
            channels: Vec::new(),
            is_self: true,
        });

        // Routing depends on this: get it wrong and our echoed messages start
        // opening a buffer named after us.
        assert_eq!(translator.me(), "myself");
    }

    #[test]
    fn a_nick_change_with_no_shared_channel_is_still_reported() {
        let mut translator = registered("me");
        let events = translator.translate(Event::NickChanged {
            old: b"me".to_vec(),
            new: b"myself".to_vec(),
            channels: Vec::new(),
            is_self: true,
        });

        assert!(
            events
                .iter()
                .any(|event| matches!(event, AppEvent::Line { .. })),
            "it would otherwise happen silently"
        );
    }

    fn roster_after(events: &[AppEvent], channel: &str) -> Option<Vec<String>> {
        events.iter().rev().find_map(|event| match event {
            AppEvent::Roster { buffer, members } if buffer == channel => Some(members.clone()),
            _ => None,
        })
    }

    fn in_channel(translator: &mut Translator, channel: &str, members: &[&str]) {
        translator.translate(Event::Names {
            channel: channel.as_bytes().to_vec(),
            members: members.iter().map(|m| m.as_bytes().to_vec()).collect(),
        });
    }

    #[test]
    fn a_nick_change_updates_the_member_list() {
        let mut translator = registered("me");
        in_channel(&mut translator, "#rust", &["@alice", "bob"]);

        let events = translator.translate(Event::NickChanged {
            old: b"bob".to_vec(),
            new: b"robert".to_vec(),
            channels: vec![b"#rust".to_vec()],
            is_self: false,
        });

        assert_eq!(
            roster_after(&events, "#rust"),
            Some(vec!["@alice".to_owned(), "robert".to_owned()]),
            "the member list has to follow the rename, not wait for a NAMES"
        );
    }

    #[test]
    fn a_rename_keeps_whatever_prefix_the_member_held() {
        let mut translator = registered("me");
        in_channel(&mut translator, "#rust", &["@alice"]);

        let events = translator.translate(Event::NickChanged {
            old: b"alice".to_vec(),
            new: b"alicia".to_vec(),
            channels: vec![b"#rust".to_vec()],
            is_self: false,
        });

        // Losing the @ would silently demote an operator in the display.
        assert_eq!(
            roster_after(&events, "#rust"),
            Some(vec!["@alicia".to_owned()])
        );
    }

    #[test]
    fn joining_and_leaving_move_the_member_list() {
        let mut translator = registered("me");
        in_channel(&mut translator, "#rust", &["@alice"]);

        let joined = translator.translate(Event::Joined {
            channel: b"#rust".to_vec(),
            who: sender("bob"),
            is_self: false,
        });
        assert_eq!(
            roster_after(&joined, "#rust"),
            Some(vec!["@alice".to_owned(), "bob".to_owned()])
        );

        let parted = translator.translate(Event::Parted {
            channel: b"#rust".to_vec(),
            who: sender("bob"),
            reason: None,
            is_self: false,
        });
        assert_eq!(
            roster_after(&parted, "#rust"),
            Some(vec!["@alice".to_owned()])
        );
    }

    #[test]
    fn quitting_leaves_every_channel_shared() {
        let mut translator = registered("me");
        in_channel(&mut translator, "#one", &["bob"]);
        in_channel(&mut translator, "#two", &["bob"]);

        let events = translator.translate(Event::Quit {
            who: sender("bob"),
            reason: None,
            channels: vec![b"#one".to_vec(), b"#two".to_vec()],
        });

        assert_eq!(roster_after(&events, "#one"), Some(Vec::new()));
        assert_eq!(roster_after(&events, "#two"), Some(Vec::new()));
    }

    #[test]
    fn a_numeric_does_not_show_its_trailing_text_twice() {
        let mut translator = registered("me");
        // 251 is "<count> :<text>": the text is also the last parameter, so a
        // client showing both prints it twice.
        let (_, line) = only_line(translator.translate(Event::Numeric {
            code: 251,
            params: vec![b"0".to_vec(), b"unknown connection(s)".to_vec()],
            text: Some(b"unknown connection(s)".to_vec()),
        }));

        assert_eq!(line.text, "0 unknown connection(s)");
    }

    #[test]
    fn a_numeric_without_trailing_text_keeps_all_its_parameters() {
        let mut translator = registered("me");
        let (_, line) = only_line(translator.translate(Event::Numeric {
            code: 4,
            params: vec![
                b"irc.example".to_vec(),
                b"kestreld".to_vec(),
                b"io".to_vec(),
            ],
            text: None,
        }));

        assert_eq!(line.text, "irc.example kestreld io");
    }

    #[test]
    fn a_refusal_reads_as_an_error() {
        let mut translator = registered("me");
        let (buffer, line) = only_line(translator.translate(Event::Numeric {
            code: 403,
            params: vec![b"#nope".to_vec()],
            text: Some(b"No such channel".to_vec()),
        }));

        assert_eq!(buffer, SERVER_BUFFER);
        assert_eq!(line.kind, LineKind::Error);
        assert!(line.text.contains("No such channel"));
    }

    #[test]
    fn an_informational_numeric_does_not_read_as_an_error() {
        let mut translator = registered("me");
        let (_, line) = only_line(translator.translate(Event::Numeric {
            code: 311,
            params: vec![b"alice".to_vec()],
            text: Some(b"is a user".to_vec()),
        }));

        assert_eq!(line.kind, LineKind::Status);
    }
}
