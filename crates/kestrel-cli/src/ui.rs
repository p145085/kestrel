//! Input handling and terminal decoration.

use anyhow::{Result, bail};
use kestrel_call::Privacy;
use kestrel_net::Handle;
use kestrel_proto::MessageBuf;

use crate::calls::Calls;

/// ANSI colours, kept here so the rest of the client reads as plain text.
pub mod colour {
    pub const RESET: &str = "\x1b[0m";
    pub const DIM: &str = "\x1b[2m";
    pub const BOLD: &str = "\x1b[1m";
    pub const GREEN: &str = "\x1b[32m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const BLUE: &str = "\x1b[34m";
    pub const CYAN: &str = "\x1b[36m";
}

/// What the client is currently looking at.
pub struct State {
    /// Where plain text is sent.
    pub target: Option<String>,
    /// Our own nickname, as the server last confirmed it.
    pub nick: String,
    /// Whether registration has completed.
    pub registered: bool,
    /// Whether the server echoes our own messages back to us.
    ///
    /// When it does, printing them locally too would show everything twice.
    pub server_echoes: bool,
    /// Input typed before registration finished.
    ///
    /// A server refuses almost everything until it has welcomed you, so a
    /// command typed during the second it takes to connect would otherwise be
    /// answered with an error rather than being carried out.
    pub pending: Vec<String>,
}

impl State {
    /// Start looking at `target`, if there is one.
    #[must_use]
    pub fn new(target: Option<String>) -> Self {
        Self {
            target,
            nick: String::new(),
            registered: false,
            server_echoes: false,
            pending: Vec::new(),
        }
    }

    /// Hold a line until the server has welcomed us.
    pub fn defer(&mut self, line: &str) {
        self.pending.push(line.to_owned());
    }

    /// Take everything that was held back.
    pub fn take_pending(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending)
    }
}

/// Print a status line.
pub fn status(text: &str) {
    println!("{}--{} {text}", colour::DIM, colour::RESET);
}

/// Print a warning.
pub fn warn(text: &str) {
    println!("{}!!{} {text}", colour::YELLOW, colour::RESET);
}

/// Ask for a password without echoing it.
///
/// There is no portable way to turn off echo without a dependency, so this is
/// honest about it rather than pretending the password is hidden.
pub fn prompt_password(prompt: &str) -> Result<String> {
    use std::io::{BufRead, Write};
    warn("the password you type will be visible");
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// Act on one line the user typed.
#[allow(clippy::too_many_lines)]
pub fn handle_input(
    line: &str,
    handle: &Handle,
    state: &mut State,
    calls: &mut Calls,
) -> Result<()> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return Ok(());
    }

    let Some(rest) = line.strip_prefix('/') else {
        // Plain text goes wherever we are looking.
        let Some(target) = state.target.clone() else {
            bail!("no target; use /join #channel or /t <nick> first");
        };
        handle.send(
            MessageBuf::new("PRIVMSG")
                .param(target.clone())
                .trailing(line),
        )?;
        echo_own_message(state, &target, line);
        return Ok(());
    };

    // `//text` escapes a message that genuinely starts with a slash.
    if let Some(text) = rest.strip_prefix('/') {
        let Some(target) = state.target.clone() else {
            bail!("no target; use /join #channel first");
        };
        let text = format!("/{text}");
        handle.send(
            MessageBuf::new("PRIVMSG")
                .param(target.clone())
                .trailing(text.clone()),
        )?;
        echo_own_message(state, &target, &text);
        return Ok(());
    }

    let (command, argument) = match rest.split_once(' ') {
        Some((command, argument)) => (command, argument.trim()),
        None => (rest, ""),
    };

    match command.to_ascii_lowercase().as_str() {
        "join" | "j" => {
            if argument.is_empty() {
                bail!("usage: /join #channel");
            }
            handle.send(MessageBuf::new("JOIN").param(argument))?;
            // Look at it straight away, so the next line typed goes there.
            state.target = argument.split(',').next().map(str::to_owned);
        }
        "part" => {
            let (channel, reason) = split_target(argument, state);
            let Some(channel) = channel else {
                bail!("usage: /part [#channel] [reason]");
            };
            let mut message = MessageBuf::new("PART").param(channel.clone());
            if !reason.is_empty() {
                message = message.trailing(reason);
            }
            handle.send(message)?;
            if state.target.as_deref() == Some(channel.as_str()) {
                state.target = None;
            }
        }
        "msg" | "m" => {
            let Some((target, text)) = argument.split_once(' ') else {
                bail!("usage: /msg <target> <text>");
            };
            handle.send(MessageBuf::new("PRIVMSG").param(target).trailing(text))?;
            echo_own_message(state, target, text);
        }
        "me" => {
            let Some(target) = state.target.clone() else {
                bail!("no target; use /join #channel first");
            };
            // CTCP ACTION, the convention every client renders as an emote.
            let action = format!("\x01ACTION {argument}\x01");
            handle.send(MessageBuf::new("PRIVMSG").param(target).trailing(action))?;
            if !state.server_echoes {
                println!(
                    "{}* {} {argument}{}",
                    colour::CYAN,
                    state.nick,
                    colour::RESET
                );
            }
        }
        "nick" => {
            if argument.is_empty() {
                bail!("usage: /nick <nickname>");
            }
            handle.send(MessageBuf::new("NICK").param(argument))?;
        }
        "topic" => {
            let (channel, topic) = split_target(argument, state);
            let Some(channel) = channel else {
                bail!("usage: /topic [#channel] [topic]");
            };
            let mut message = MessageBuf::new("TOPIC").param(channel);
            if !topic.is_empty() {
                message = message.trailing(topic);
            }
            handle.send(message)?;
        }
        "names" => {
            let channel = if argument.is_empty() {
                state.target.clone()
            } else {
                Some(argument.to_owned())
            };
            let Some(channel) = channel else {
                bail!("usage: /names [#channel]");
            };
            handle.send(MessageBuf::new("NAMES").param(channel))?;
        }
        "whois" => {
            if argument.is_empty() {
                bail!("usage: /whois <nick>");
            }
            handle.send(MessageBuf::new("WHOIS").param(argument))?;
        }
        "t" | "target" => {
            if argument.is_empty() {
                match &state.target {
                    Some(target) => status(&format!("talking to {target}")),
                    None => status("no target"),
                }
            } else {
                state.target = Some(argument.to_owned());
                status(&format!("now talking to {argument}"));
            }
        }
        "raw" | "quote" => {
            if argument.is_empty() {
                bail!("usage: /raw <protocol line>");
            }
            handle.send_raw(argument)?;
        }
        "call" => {
            let target = if argument.is_empty() {
                state.target.clone().unwrap_or_default()
            } else {
                argument.to_owned()
            };
            if target.is_empty() {
                bail!("usage: /call <nick or #channel>");
            }
            calls.start(handle, &target)?;
        }
        "answer" => calls.answer(handle)?,
        "reject" => calls.reject(handle)?,
        "hangup" => calls.hang_up(handle)?,
        "verify" => calls.verify()?,
        "relayonly" => {
            let on = !matches!(argument, "off" | "no" | "false");
            calls.set_privacy(if on {
                Privacy::RelayOnly
            } else {
                Privacy::Direct
            });
        }
        "quit" => {
            let reason = if argument.is_empty() {
                "Leaving"
            } else {
                argument
            };
            handle.quit(reason)?;
        }
        other => bail!("unknown command /{other}"),
    }
    Ok(())
}

/// Show a message we just sent.
///
/// Without `echo-message` the server never sends our own words back, so a
/// client that showed only what arrives would look like it had swallowed them.
/// With it, the server's copy is the one to show — it is the authoritative
/// version, and printing ours as well would double every line.
fn echo_own_message(state: &State, target: &str, text: &str) {
    if state.server_echoes {
        return;
    }
    println!(
        "{}{target}{} <{}{}{}> {text}",
        colour::DIM,
        colour::RESET,
        colour::BOLD,
        state.nick,
        colour::RESET
    );
}

/// Split `[#channel] [rest]`, falling back to the current target.
fn split_target(argument: &str, state: &State) -> (Option<String>, String) {
    if argument.starts_with('#') || argument.starts_with('&') {
        match argument.split_once(' ') {
            Some((channel, rest)) => (Some(channel.to_owned()), rest.trim().to_owned()),
            None => (Some(argument.to_owned()), String::new()),
        }
    } else {
        (state.target.clone(), argument.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::{State, split_target};

    fn state_with(target: Option<&str>) -> State {
        State::new(target.map(str::to_owned))
    }

    #[test]
    fn an_explicit_channel_is_taken_from_the_argument() {
        let state = state_with(Some("#current"));
        assert_eq!(
            split_target("#other the topic", &state),
            (Some("#other".to_owned()), "the topic".to_owned())
        );
    }

    #[test]
    fn a_bare_channel_leaves_no_remainder() {
        let state = state_with(Some("#current"));
        assert_eq!(
            split_target("#other", &state),
            (Some("#other".to_owned()), String::new())
        );
    }

    #[test]
    fn without_a_channel_the_current_target_is_used() {
        let state = state_with(Some("#current"));
        assert_eq!(
            split_target("the topic", &state),
            (Some("#current".to_owned()), "the topic".to_owned())
        );
    }

    #[test]
    fn with_no_target_at_all_there_is_nothing_to_act_on() {
        let state = state_with(None);
        assert_eq!(
            split_target("the topic", &state),
            (None, "the topic".to_owned())
        );
    }
}
