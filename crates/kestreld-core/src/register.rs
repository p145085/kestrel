//! Registration: `CAP`, `PASS`, `NICK`, `USER`, and the welcome burst.

use kestrel_proto::{Message, MessageBuf, numeric};

use crate::client::ClientId;
use crate::datetime::format_utc;
use crate::server::{Action, Server};

/// Capabilities this server offers, as `name` or `name=value`.
///
/// The rest of the IRCv3 set, and the call capability, land with the crates
/// that implement them. Advertising a capability we do not honour is worse
/// than advertising none: a client that enables it changes its own behaviour
/// and then waits for messages that never arrive.
fn supported_caps() -> Vec<String> {
    vec![format!(
        "sasl={}",
        kestreld_services::Mechanism::advertised()
    )]
}

/// The bare name of a capability token, dropping any `=value`.
fn cap_name(token: &str) -> &str {
    token.split('=').next().unwrap_or(token)
}

/// Whether `name` is a capability this server offers.
fn is_supported(name: &str) -> bool {
    supported_caps().iter().any(|token| cap_name(token) == name)
}

impl Server {
    pub(crate) fn cmd_cap(
        &mut self,
        id: ClientId,
        msg: &Message<'_>,
        now: u64,
        out: &mut Vec<Action>,
    ) {
        let Some(subcommand) = msg.param(0) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NEEDMOREPARAMS)
                    .param("CAP")
                    .trailing("Not enough parameters"),
            });
            return;
        };

        // The target of a CAP reply is the nick, or `*` before one is chosen.
        let target = self
            .client(id)
            .map_or(&b"*"[..], crate::client::Client::nick_or_star)
            .to_vec();

        match subcommand.to_ascii_uppercase().as_slice() {
            b"LS" => {
                // Asking for capabilities suspends registration until CAP END,
                // so a client can finish negotiating before it is welcomed.
                self.set_cap_negotiating(id, true);
                out.push(Action::Send {
                    to: id,
                    message: MessageBuf::new("CAP")
                        .source(self.server_name())
                        .param(target)
                        .param("LS")
                        .trailing(supported_caps().join(" ")),
                });
            }
            b"LIST" => {
                let mut enabled: Vec<String> = self
                    .client(id)
                    .map(|c| c.caps().iter().cloned().collect())
                    .unwrap_or_default();
                enabled.sort();
                out.push(Action::Send {
                    to: id,
                    message: MessageBuf::new("CAP")
                        .source(self.server_name())
                        .param(target)
                        .param("LIST")
                        .trailing(enabled.join(" ")),
                });
            }
            b"REQ" => {
                let requested = msg.param(1).unwrap_or(b"").to_vec();
                self.set_cap_negotiating(id, true);
                self.handle_cap_req(id, &target, &requested, out);
            }
            b"END" => {
                self.set_cap_negotiating(id, false);
                // The client may have sent NICK and USER during negotiation,
                // in which case this is what unblocks registration.
                self.try_complete_registration(id, now, out);
            }
            _ => {
                out.push(Action::Send {
                    to: id,
                    message: self
                        .numeric(id, numeric::ERR_UNKNOWNCOMMAND)
                        .param("CAP")
                        .trailing("Invalid CAP subcommand"),
                });
            }
        }
    }

    /// Handle `CAP REQ`.
    ///
    /// A request is all-or-nothing: if any capability in the list is refused,
    /// none are enabled. Partially applying a request would leave the client
    /// and server disagreeing about which are active.
    fn handle_cap_req(
        &mut self,
        id: ClientId,
        target: &[u8],
        requested: &[u8],
        out: &mut Vec<Action>,
    ) {
        let text = String::from_utf8_lossy(requested).into_owned();
        let tokens: Vec<&str> = text.split_whitespace().collect();

        let all_known = !tokens.is_empty()
            && tokens
                .iter()
                .all(|token| is_supported(token.trim_start_matches('-')));

        let verb = if all_known { "ACK" } else { "NAK" };
        if all_known {
            for token in &tokens {
                if let Some(name) = token.strip_prefix('-') {
                    self.disable_cap(id, name);
                } else {
                    self.enable_cap(id, token);
                }
            }
        }

        // ACK and NAK both echo the request verbatim, so the client can match
        // the reply to what it asked for.
        out.push(Action::Send {
            to: id,
            message: MessageBuf::new("CAP")
                .source(self.server_name())
                .param(target.to_vec())
                .param(verb)
                .trailing(requested.to_vec()),
        });
    }

    pub(crate) fn cmd_pass(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        if self
            .client(id)
            .is_some_and(crate::client::Client::is_registered)
        {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_ALREADYREGISTERED)
                    .trailing("You may not reregister"),
            });
            return;
        }
        let Some(supplied) = msg.param(0) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NEEDMOREPARAMS)
                    .param("PASS")
                    .trailing("Not enough parameters"),
            });
            return;
        };

        // Compared in constant time so a wrong password cannot be narrowed
        // down by timing how long the rejection takes.
        let matches = self
            .config()
            .password
            .as_deref()
            .is_some_and(|expected| constant_time_eq(expected, supplied));
        self.set_password_ok(id, matches);
    }

    pub(crate) fn cmd_nick(
        &mut self,
        id: ClientId,
        msg: &Message<'_>,
        now: u64,
        out: &mut Vec<Action>,
    ) {
        let Some(requested) = msg.param(0).filter(|n| !n.is_empty()) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NONICKNAMEGIVEN)
                    .trailing("No nickname given"),
            });
            return;
        };

        if !self.config().is_valid_nick(requested) {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_ERRONEUSNICKNAME)
                    .param(requested.to_vec())
                    .trailing("Erroneous nickname"),
            });
            return;
        }

        // Taking a nick you already hold, in a different case, is a rename.
        // Taking one somebody else holds is a collision.
        match self.find_nick(requested) {
            Some(holder) if holder != id => {
                out.push(Action::Send {
                    to: id,
                    message: self
                        .numeric(id, numeric::ERR_NICKNAMEINUSE)
                        .param(requested.to_vec())
                        .trailing("Nickname is already in use"),
                });
                return;
            }
            _ => {}
        }

        let was_registered = self
            .client(id)
            .is_some_and(crate::client::Client::is_registered);
        let old_mask = self.client(id).map(crate::client::Client::mask);

        self.set_nick(id, requested);

        if was_registered {
            // Announce with the old mask, so recipients can match the change
            // against the nick they already know.
            let announcement = MessageBuf::new("NICK")
                .source(old_mask.unwrap_or_default())
                .param(requested.to_vec());
            out.push(Action::Send {
                to: id,
                message: announcement.clone(),
            });
            for peer in self.peers_of(id) {
                out.push(Action::Send {
                    to: peer,
                    message: announcement.clone(),
                });
            }
        } else {
            self.try_complete_registration(id, now, out);
        }
    }

    pub(crate) fn cmd_user(
        &mut self,
        id: ClientId,
        msg: &Message<'_>,
        now: u64,
        out: &mut Vec<Action>,
    ) {
        if self
            .client(id)
            .is_some_and(crate::client::Client::is_registered)
        {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_ALREADYREGISTERED)
                    .trailing("You may not reregister"),
            });
            return;
        }

        // USER <username> <mode> <unused> :<realname>
        let (Some(username), Some(realname)) = (msg.param(0), msg.param(3)) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NEEDMOREPARAMS)
                    .param("USER")
                    .trailing("Not enough parameters"),
            });
            return;
        };

        self.set_user(id, username, realname);
        self.try_complete_registration(id, now, out);
    }

    /// Send everything a client receives immediately after registering.
    pub(crate) fn send_welcome(&self, id: ClientId, out: &mut Vec<Action>) {
        let Some(client) = self.client(id) else {
            return;
        };
        let mask = client.mask();
        let config = self.config();
        let network = String::from_utf8_lossy(&config.network_name).into_owned();
        let server_name = String::from_utf8_lossy(&config.server_name).into_owned();
        let version = String::from_utf8_lossy(&config.version).into_owned();

        let push = |out: &mut Vec<Action>, message: MessageBuf| {
            out.push(Action::Send { to: id, message });
        };

        push(
            out,
            self.numeric(id, numeric::RPL_WELCOME).trailing(format!(
                "Welcome to the {network} IRC Network, {}",
                String::from_utf8_lossy(&mask)
            )),
        );
        push(
            out,
            self.numeric(id, numeric::RPL_YOURHOST).trailing(format!(
                "Your host is {server_name}, running version {version}"
            )),
        );
        push(
            out,
            self.numeric(id, numeric::RPL_CREATED).trailing(format!(
                "This server was created {}",
                format_utc(self.created_at())
            )),
        );
        push(
            out,
            self.numeric(id, numeric::RPL_MYINFO)
                .param(server_name)
                .param(version)
                // Available user modes, then channel modes.
                .param("io")
                .param("biklmnst"),
        );

        for chunk in self.isupport_tokens().chunks(13) {
            let mut reply = self.numeric(id, numeric::RPL_ISUPPORT);
            for token in chunk {
                reply = reply.param(token.clone());
            }
            push(out, reply.trailing("are supported by this server"));
        }

        self.send_lusers(id, out);
        self.send_motd(id, out);
    }

    /// The `RPL_ISUPPORT` tokens this server advertises.
    fn isupport_tokens(&self) -> Vec<String> {
        let config = self.config();
        let casemapping = match config.casemapping {
            kestrel_proto::CaseMapping::Ascii => "ascii",
            kestrel_proto::CaseMapping::Rfc1459 => "rfc1459",
            kestrel_proto::CaseMapping::Rfc1459Strict => "rfc1459-strict",
        };
        let prefixes = String::from_utf8_lossy(&config.channel_prefixes).into_owned();
        vec![
            format!("NETWORK={}", String::from_utf8_lossy(&config.network_name)),
            format!("CASEMAPPING={casemapping}"),
            format!("CHANTYPES={prefixes}"),
            // Modes taking a parameter always, on set only, and never.
            "CHANMODES=b,k,l,imnst".to_string(),
            "PREFIX=(ov)@+".to_string(),
            format!("NICKLEN={}", config.max_nick_len),
            format!("CHANNELLEN={}", config.max_channel_len),
            format!("TOPICLEN={}", config.max_topic_len),
            format!("CHANLIMIT={prefixes}:{}", config.max_channels_per_client),
            "MODES=20".to_string(),
            "MAXTARGETS=4".to_string(),
            "LINELEN=512".to_string(),
            "SAFELIST".to_string(),
            "STATUSMSG=@+".to_string(),
        ]
    }

    pub(crate) fn send_lusers(&self, id: ClientId, out: &mut Vec<Action>) {
        let registered = self.registered_count();
        let unknown = self.client_count() - registered;
        let channels = self.channel_count();

        let push = |out: &mut Vec<Action>, message: MessageBuf| {
            out.push(Action::Send { to: id, message });
        };

        push(
            out,
            self.numeric(id, numeric::RPL_LUSERCLIENT).trailing(format!(
                "There are {registered} users and 0 invisible on 1 servers"
            )),
        );
        push(
            out,
            self.numeric(id, numeric::RPL_LUSERUNKNOWN)
                .param(unknown.to_string())
                .trailing("unknown connection(s)"),
        );
        push(
            out,
            self.numeric(id, numeric::RPL_LUSERCHANNELS)
                .param(channels.to_string())
                .trailing("channels formed"),
        );
        push(
            out,
            self.numeric(id, numeric::RPL_LUSERME)
                .trailing(format!("I have {registered} clients and 0 servers")),
        );
        push(
            out,
            self.numeric(id, numeric::RPL_LOCALUSERS)
                .param(registered.to_string())
                .param(self.max_local_users().to_string())
                .trailing(format!(
                    "Current local users {registered}, max {}",
                    self.max_local_users()
                )),
        );
    }

    pub(crate) fn send_motd(&self, id: ClientId, out: &mut Vec<Action>) {
        let motd = &self.config().motd;
        if motd.is_empty() {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NOMOTD)
                    .trailing("MOTD File is missing"),
            });
            return;
        }

        out.push(Action::Send {
            to: id,
            message: self.numeric(id, numeric::RPL_MOTDSTART).trailing(format!(
                "- {} Message of the day -",
                String::from_utf8_lossy(&self.config().server_name)
            )),
        });
        for line in motd {
            let mut text = b"- ".to_vec();
            text.extend_from_slice(line);
            out.push(Action::Send {
                to: id,
                message: self.numeric(id, numeric::RPL_MOTD).trailing(text),
            });
        }
        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_ENDOFMOTD)
                .trailing("End of /MOTD command"),
        });
    }
}

/// Compare two byte strings without leaking their contents through timing.
///
/// The length is allowed to leak — it is not secret — but the comparison
/// itself always examines every byte of the longer input.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn constant_time_eq_matches_normal_equality() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"hunter2", b"hunter2"));
        assert!(!constant_time_eq(b"hunter2", b"hunter3"));
        assert!(!constant_time_eq(b"short", b"longer"));
        assert!(!constant_time_eq(b"", b"x"));
    }
}
