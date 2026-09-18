//! What the user typed, turned into what should happen.
//!
//! Pure, and deliberately so: command parsing is where a client is most often
//! subtly wrong, and none of it needs a socket or a display to test.

use kestrel_proto::{Message, MessageBuf};

use crate::event::{BufferId, Line, SERVER_BUFFER};

/// One consequence of something the user typed.
#[derive(Debug)]
pub enum Action {
    /// Send this to the server.
    Send(Box<MessageBuf>),
    /// Show this locally, in this buffer.
    Show(BufferId, Line),
    /// Open a buffer and bring it forward.
    Open(BufferId),
    /// Leave.
    Quit(String),
}

/// What `/help` prints.
///
/// Listed here rather than in the interface so the terminal and the window
/// describe the same commands, and so a command cannot be added without a
/// visible place to document it.
const HELP: [&str; 13] = [
    "/join #channel      join a channel",
    "/part [#channel]    leave a channel, or this one",
    "/msg nick text      send a private message",
    "/query nick         open a conversation without sending anything",
    "/nick name          change your nickname",
    "/me does something  say something in the third person",
    "/topic [text]       show or set the channel topic",
    "/names [#channel]   refresh the member list",
    "/whois nick         ask about somebody",
    "/raw LINE           send a raw IRC line",
    "/register acc pass  create an account on this server",
    "/quit [reason]      disconnect and leave",
    "//text              say something starting with a slash",
];

/// Whether a name is a channel rather than a nickname.
fn is_channel(name: &str) -> bool {
    name.starts_with('#') || name.starts_with('&')
}

/// Turn one line of input into the actions it implies.
///
/// `echoed` says whether the server will send our own messages back to us. When
/// it will, showing them locally too would print everything twice -- which is
/// what happens in clients that assume one or the other unconditionally.
// One arm per command, which is long but flat: splitting it would scatter
// the vocabulary across several functions for no gain in clarity.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn parse(buffer: &str, me: &str, input: &str, echoed: bool) -> Vec<Action> {
    let input = input.trim_end_matches(['\r', '\n']);
    if input.is_empty() {
        return Vec::new();
    }

    let Some(rest) = input.strip_prefix('/') else {
        return say(buffer, me, input, echoed);
    };

    // "//text" is how you say something that starts with a slash, so what
    // is said keeps the second slash: only the escaping one is removed.
    if rest.starts_with('/') {
        return say(buffer, me, rest, echoed);
    }

    let (verb, args) = rest.split_once(' ').unwrap_or((rest, ""));
    let args = args.trim();

    match verb.to_ascii_lowercase().as_str() {
        "join" | "j" => {
            if args.is_empty() {
                return vec![refuse(buffer, "/join needs a channel")];
            }
            args.split(',')
                .filter(|name| !name.is_empty())
                .map(|name| Action::Send(Box::new(MessageBuf::new("JOIN").param(name))))
                .collect()
        }
        "part" | "leave" => {
            let target = if args.is_empty() { buffer } else { args };
            if target.is_empty() {
                return vec![refuse(buffer, "/part needs a channel")];
            }
            vec![Action::Send(Box::new(
                MessageBuf::new("PART").param(target),
            ))]
        }
        "msg" | "query" => {
            let (target, body) = args.split_once(' ').unwrap_or((args, ""));
            if target.is_empty() {
                return vec![refuse(buffer, "/msg needs somebody to talk to")];
            }
            // Opening the conversation even with nothing to say is the point of
            // /query, and harmless for /msg.
            let mut out = vec![Action::Open(target.to_owned())];
            if !body.trim().is_empty() {
                out.extend(say(target, me, body.trim(), echoed));
            }
            out
        }
        "nick" => {
            if args.is_empty() {
                return vec![refuse(buffer, "/nick needs a nickname")];
            }
            vec![Action::Send(Box::new(MessageBuf::new("NICK").param(args)))]
        }
        "me" => {
            if buffer.is_empty() {
                return vec![refuse(buffer, "there is nobody here to act at")];
            }
            if args.is_empty() {
                return vec![refuse(buffer, "/me needs something to do")];
            }
            let body = format!("\u{1}ACTION {args}\u{1}");
            let mut out = vec![Action::Send(Box::new(
                MessageBuf::new("PRIVMSG").param(buffer).trailing(&*body),
            ))];
            if !echoed {
                out.push(Action::Show(buffer.to_owned(), Line::action(me, args)));
            }
            out
        }
        "topic" => {
            if !is_channel(buffer) {
                return vec![refuse(buffer, "/topic only means something in a channel")];
            }
            let message = if args.is_empty() {
                MessageBuf::new("TOPIC").param(buffer)
            } else {
                MessageBuf::new("TOPIC").param(buffer).trailing(args)
            };
            vec![Action::Send(Box::new(message))]
        }
        "names" => {
            let target = if args.is_empty() { buffer } else { args };
            if !is_channel(target) {
                return vec![refuse(buffer, "/names needs a channel")];
            }
            vec![Action::Send(Box::new(
                MessageBuf::new("NAMES").param(target),
            ))]
        }
        "whois" => {
            if args.is_empty() {
                return vec![refuse(buffer, "/whois needs a nickname")];
            }
            vec![Action::Send(Box::new(MessageBuf::new("WHOIS").param(args)))]
        }
        "raw" | "quote" => {
            if args.is_empty() {
                return vec![refuse(buffer, "/raw needs a line to send")];
            }
            // A line terminator in the middle would let one /raw put two
            // commands on the wire. The serialiser refuses such a message
            // anyway, but refusing here says so while the user can still
            // see what they typed, instead of failing later as "could not
            // send" with nothing to point at.
            if args.contains(['\r', '\n', '\0']) {
                return vec![refuse(buffer, "a raw line cannot contain a line break")];
            }
            // Parsed rather than passed through, so what reaches the wire
            // is something this client could itself have produced.
            match Message::parse(args.as_bytes()) {
                Ok(message) => vec![Action::Send(Box::new(MessageBuf::from(&message)))],
                Err(_) => vec![refuse(buffer, "that is not a valid IRC line")],
            }
        }
        "help" => HELP
            .iter()
            .map(|entry| Action::Show(buffer.to_owned(), Line::status(*entry)))
            .collect(),
        "register" => {
            let (account, password) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
            if account.is_empty() || password.trim().is_empty() {
                return vec![refuse(buffer, "/register needs an account and a password")];
            }
            // The email field is required by the specification and
            // unused here; `*` is how you decline to give one.
            vec![Action::Send(Box::new(
                MessageBuf::new("REGISTER")
                    .param(account)
                    .param("*")
                    .param(password.trim()),
            ))]
        }
        "quit" => {
            let reason = if args.is_empty() { "kestrel" } else { args };
            vec![Action::Quit(reason.to_owned())]
        }
        other => vec![refuse(buffer, format!("no such command: /{other}"))],
    }
}

/// Say something in a buffer.
fn say(buffer: &str, me: &str, body: &str, echoed: bool) -> Vec<Action> {
    if buffer.is_empty() {
        // The server buffer has no correspondent, so there is nowhere to send
        // this. Saying so beats swallowing it.
        return vec![refuse(
            buffer,
            "this is the server buffer; join a channel or use /msg",
        )];
    }

    let mut out = vec![Action::Send(Box::new(
        MessageBuf::new("PRIVMSG").param(buffer).trailing(body),
    ))];
    if !echoed {
        out.push(Action::Show(buffer.to_owned(), Line::own(me, body)));
    }
    out
}

/// Tell the user why nothing happened, where they were looking.
fn refuse(buffer: &str, why: impl Into<String>) -> Action {
    let where_to = if buffer.is_empty() {
        SERVER_BUFFER.to_owned()
    } else {
        buffer.to_owned()
    };
    Action::Show(where_to, Line::error(why))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Send(message) => Some(
                    String::from_utf8_lossy(&message.to_vec().expect("should serialise"))
                        .into_owned(),
                ),
                _ => None,
            })
            .collect()
    }

    fn shown(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Show(_, line) => Some(line.text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn plain_text_is_a_message_to_the_current_buffer() {
        let actions = parse("#rust", "me", "hello", true);
        assert_eq!(sent(&actions), ["PRIVMSG #rust :hello\r\n"]);
    }

    #[test]
    fn our_own_message_is_shown_locally_only_when_the_server_will_not_echo_it() {
        // The whole point: doing both is how a client prints everything twice.
        assert!(shown(&parse("#rust", "me", "hi", true)).is_empty());
        assert_eq!(shown(&parse("#rust", "me", "hi", false)), ["hi"]);
    }

    #[test]
    fn a_doubled_slash_says_something_starting_with_a_slash() {
        let actions = parse("#rust", "me", "//join is a command", true);
        assert_eq!(sent(&actions), ["PRIVMSG #rust :/join is a command\r\n"]);
    }

    #[test]
    fn an_action_travels_as_ctcp() {
        let actions = parse("#rust", "me", "/me waves", true);
        assert_eq!(
            sent(&actions),
            ["PRIVMSG #rust :\u{1}ACTION waves\u{1}\r\n"]
        );
    }

    #[test]
    fn join_takes_a_comma_separated_list() {
        let actions = parse("", "me", "/join #a,#b", true);
        assert_eq!(sent(&actions), ["JOIN #a\r\n", "JOIN #b\r\n"]);
    }

    #[test]
    fn part_defaults_to_the_current_channel() {
        let actions = parse("#rust", "me", "/part", true);
        assert_eq!(sent(&actions), ["PART #rust\r\n"]);
    }

    #[test]
    fn query_opens_a_conversation_without_saying_anything() {
        let actions = parse("", "me", "/query alice", true);
        assert!(sent(&actions).is_empty(), "nothing to send yet");
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::Open(name) if name == "alice"))
        );
    }

    #[test]
    fn msg_opens_the_conversation_and_sends() {
        let actions = parse("", "me", "/msg alice hello there", true);
        assert_eq!(sent(&actions), ["PRIVMSG alice :hello there\r\n"]);
    }

    #[test]
    fn saying_something_in_the_server_buffer_is_refused_rather_than_swallowed() {
        let actions = parse(SERVER_BUFFER, "me", "hello", true);
        assert!(sent(&actions).is_empty());
        assert_eq!(shown(&actions).len(), 1, "the user must be told why");
    }

    #[test]
    fn a_raw_line_with_an_embedded_newline_cannot_smuggle_a_second_command() {
        let actions = parse("", "me", "/raw PRIVMSG #a :hi\r\nJOIN #secret", true);
        assert!(sent(&actions).is_empty(), "nothing may reach the wire");
        assert_eq!(
            shown(&actions),
            ["a raw line cannot contain a line break"],
            "and the user is told why, rather than it failing at send time"
        );
    }

    #[test]
    fn help_lists_the_commands_and_is_not_itself_unknown() {
        // The entry box advertises /help, so it not existing was worse than a
        // missing feature: it was the client contradicting itself.
        let actions = parse("#rust", "me", "/help", true);
        assert!(sent(&actions).is_empty(), "help is answered locally");

        let shown = shown(&actions);
        assert_eq!(shown.len(), HELP.len());
        for command in ["/join", "/part", "/msg", "/nick", "/me", "/quit"] {
            assert!(
                shown.iter().any(|line| line.starts_with(command)),
                "{command} is not documented"
            );
        }
    }

    #[test]
    fn every_documented_command_is_understood() {
        // A help text that lists something the parser rejects is worse than no
        // help at all, so the two are checked against each other.
        for entry in HELP {
            let verb = entry.split_whitespace().next().expect("an entry");
            if verb == "//text" {
                continue;
            }
            let actions = parse("#rust", "me", verb, true);
            let refused = shown(&actions)
                .iter()
                .any(|line| line.starts_with("no such command"));
            assert!(!refused, "{verb} is documented but not understood");
        }
    }

    #[test]
    fn registering_needs_both_halves() {
        // Half a registration reaching the server would create an account
        // whose password the user does not know they did not choose.
        assert!(sent(&parse("", "me", "/register alice", true)).is_empty());
        assert_eq!(
            sent(&parse("", "me", "/register alice secret", true)),
            ["REGISTER alice * secret\r\n"]
        );
    }

    #[test]
    fn an_unknown_command_says_so() {
        let actions = parse("#rust", "me", "/frobnicate", true);
        assert!(sent(&actions).is_empty());
        assert_eq!(shown(&actions), ["no such command: /frobnicate"]);
    }

    #[test]
    fn topic_outside_a_channel_is_refused() {
        let actions = parse("alice", "me", "/topic hello", true);
        assert!(sent(&actions).is_empty());
    }
}
