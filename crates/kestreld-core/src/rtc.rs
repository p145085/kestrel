//! The `CALL` command family.
//!
//! See `spec/rtc-over-irc.md`. The server routes signalling and tracks who is
//! in which call; it never sees media, and the payloads it relays are sealed
//! to a specific recipient, so it cannot read those either.

use kestrel_proto::{Message, MessageBuf};

use crate::calls::{CallId, Media};
use crate::client::{Client, ClientId};
use crate::server::{Action, Server};

/// Protocol version this server speaks.
pub const VERSION: u32 = 0;

/// Largest signalling payload accepted, in bytes.
///
/// A compact session description is roughly 200 bytes, so this is generous
/// enough that no legitimate exchange approaches it while still bounding what
/// one client can make the server relay.
pub const MAX_SIGNAL: usize = 8192;

/// Largest mesh call permitted.
///
/// Above this the bandwidth each participant must upload — one stream per
/// other participant — stops being something an ordinary connection can carry.
/// Refusing is more honest than admitting someone to a call that will not work.
pub const MAX_MESH: usize = 8;

/// The capability token, advertised when calls are enabled.
#[must_use]
pub fn capability_token() -> String {
    format!("kestrel.chat/rtc=ver={VERSION},maxsig={MAX_SIGNAL},maxmesh={MAX_MESH}")
}

impl Server {
    pub(crate) fn cmd_call(
        &mut self,
        id: ClientId,
        msg: &Message<'_>,
        now: u64,
        out: &mut Vec<Action>,
    ) {
        if !self.config().calls_enabled {
            self.fail_call(id, "RTC_DISABLED", None, "Calls are not enabled here", out);
            return;
        }
        // A client that has not negotiated the capability has no way to be told
        // what happens next, so it must not be able to start anything.
        if !self
            .client(id)
            .is_some_and(|c| c.has_cap("kestrel.chat/rtc"))
        {
            self.fail_call(
                id,
                "RTC_DISABLED",
                None,
                "Request the kestrel.chat/rtc capability first",
                out,
            );
            return;
        }

        let Some(subcommand) = msg.param(0) else {
            self.fail_call(id, "NEED_MORE_PARAMS", None, "Not enough parameters", out);
            return;
        };

        match subcommand.to_ascii_uppercase().as_slice() {
            b"START" => self.call_start(id, msg, now, out),
            b"INVITE" => self.call_invite(id, msg, out),
            b"ACCEPT" => self.call_accept(id, msg, out),
            b"DECLINE" => self.call_decline(id, msg, out),
            b"LEAVE" => self.call_leave(id, msg, out),
            b"LIST" => self.call_list(id, msg, out),
            b"SIGNAL" => self.call_signal(id, msg, out),
            _ => self.fail_call(
                id,
                "INVALID_SUBCOMMAND",
                None,
                "Unknown CALL subcommand",
                out,
            ),
        }
    }

    // --- starting and joining ---------------------------------------------

    fn call_start(&mut self, id: ClientId, msg: &Message<'_>, now: u64, out: &mut Vec<Action>) {
        let Some(target) = msg.param(1).filter(|t| !t.is_empty()) else {
            self.fail_call(id, "NEED_MORE_PARAMS", None, "Give a channel or nick", out);
            return;
        };
        let media = msg.param(2).map_or_else(Media::audio_video, Media::parse);
        let folded = self.fold(target);
        let in_channel = self.config().is_valid_channel(target);

        // A call in a channel is subject to that channel's rules: someone who
        // may not speak there may not call there either.
        let display = if in_channel {
            let Some(channel) = self.channel(target) else {
                self.fail_call(id, "NO_SUCH_TARGET", None, "No such channel", out);
                return;
            };
            if !channel.contains(id) {
                self.fail_call(
                    id,
                    "NOT_IN_CHANNEL",
                    None,
                    "You are not on that channel",
                    out,
                );
                return;
            }
            channel.name().to_vec()
        } else {
            let Some(other) = self.find_nick(target) else {
                self.fail_call(id, "NO_SUCH_TARGET", None, "No such nick", out);
                return;
            };
            let _ = other;
            target.to_vec()
        };

        let existing = self.calls().by_target(&folded).map(|c| (c.id(), c.len()));
        let call_id = match existing {
            Some((call_id, participants)) => {
                if participants >= MAX_MESH {
                    self.fail_call(id, "CALL_FULL", Some(call_id), "That call is full", out);
                    return;
                }
                call_id
            }
            None => self
                .calls_mut()
                .create(&folded, display.clone(), in_channel, media, now),
        };

        if let Some(call) = self.calls_mut().get_mut(call_id) {
            call.add(id, true);
        }

        out.push(Action::Send {
            to: id,
            message: MessageBuf::new("CALL")
                .source(self.server_name())
                .param(call_id.to_wire())
                .param("STARTED")
                .param(display.clone())
                .param(media.to_wire()),
        });

        let Some(mask) = self.client(id).map(Client::mask) else {
            return;
        };
        let joined = MessageBuf::new("CALL")
            .source(mask)
            .param(call_id.to_wire())
            .param("JOINED")
            .param(display);
        self.announce_to_call(call_id, &joined, Some(id), out);

        // A one-to-one call invites the other side rather than adding them:
        // being called is not the same as answering.
        if !in_channel && let Some(other) = self.find_nick(target) {
            self.send_invite(id, other, call_id, out);
        }
    }

    fn call_invite(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let Some((call_id, nick)) = self.call_and_param(id, msg, out) else {
            return;
        };
        let Some(target) = self.find_nick(&nick) else {
            self.fail_call(id, "NO_SUCH_TARGET", Some(call_id), "No such nick", out);
            return;
        };
        if !self
            .calls()
            .get(call_id)
            .is_some_and(|c| c.has_accepted(id))
        {
            self.fail_call(
                id,
                "NOT_IN_CALL",
                Some(call_id),
                "You are not in that call",
                out,
            );
            return;
        }
        self.send_invite(id, target, call_id, out);
    }

    fn send_invite(
        &mut self,
        from: ClientId,
        to: ClientId,
        call_id: CallId,
        out: &mut Vec<Action>,
    ) {
        let Some(call) = self.calls().get(call_id) else {
            return;
        };
        let (target, media) = (call.target().to_vec(), call.media());
        if let Some(call) = self.calls_mut().get_mut(call_id) {
            call.add(to, false);
        }

        let Some(mask) = self.client(from).map(Client::mask) else {
            return;
        };
        // A client that cannot understand the invitation is not told about it.
        if self
            .client(to)
            .is_some_and(|c| c.has_cap("kestrel.chat/rtc"))
        {
            out.push(Action::Send {
                to,
                message: MessageBuf::new("CALL")
                    .source(mask)
                    .param(call_id.to_wire())
                    .param("INVITE")
                    .param(target)
                    .param(media.to_wire()),
            });
        }
    }

    fn call_accept(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let Some(call_id) = self.call_id_param(id, msg, out) else {
            return;
        };
        let Some(call) = self.calls().get(call_id) else {
            return;
        };
        if !call.contains(id) {
            self.fail_call(
                id,
                "NOT_IN_CALL",
                Some(call_id),
                "You were not invited to that call",
                out,
            );
            return;
        }
        if call.len() >= MAX_MESH && !call.has_accepted(id) {
            self.fail_call(id, "CALL_FULL", Some(call_id), "That call is full", out);
            return;
        }

        let target = call.target().to_vec();
        if let Some(call) = self.calls_mut().get_mut(call_id) {
            call.add(id, true);
        }

        let Some(mask) = self.client(id).map(Client::mask) else {
            return;
        };
        let joined = MessageBuf::new("CALL")
            .source(mask)
            .param(call_id.to_wire())
            .param("JOINED")
            .param(target);
        self.announce_to_call(call_id, &joined, None, out);
    }

    fn call_decline(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let Some(call_id) = self.call_id_param(id, msg, out) else {
            return;
        };
        let reason = msg.param(2).map(<[u8]>::to_vec);
        self.depart_call(id, call_id, reason, "DECLINED", out);
    }

    fn call_leave(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let Some(call_id) = self.call_id_param(id, msg, out) else {
            return;
        };
        if !self.calls().get(call_id).is_some_and(|c| c.contains(id)) {
            self.fail_call(
                id,
                "NOT_IN_CALL",
                Some(call_id),
                "You are not in that call",
                out,
            );
            return;
        }
        let reason = msg.param(2).map(<[u8]>::to_vec);
        self.depart_call(id, call_id, reason, "LEFT", out);
    }

    /// Remove a client from a call and tell whoever is left.
    pub(crate) fn depart_call(
        &mut self,
        id: ClientId,
        call_id: CallId,
        reason: Option<Vec<u8>>,
        verb: &str,
        out: &mut Vec<Action>,
    ) {
        let Some(call) = self.calls().get(call_id) else {
            return;
        };
        let target = call.target().to_vec();
        let Some(mask) = self.client(id).map(Client::mask) else {
            return;
        };

        if let Some(call) = self.calls_mut().get_mut(call_id) {
            call.remove(id);
        }

        let mut message = MessageBuf::new("CALL")
            .source(mask)
            .param(call_id.to_wire())
            .param(verb)
            .param(target);
        if let Some(reason) = reason {
            message = message.trailing(reason);
        }
        self.announce_to_call(call_id, &message, None, out);
        self.calls_mut().drop_if_empty(call_id);
    }

    // --- discovery ---------------------------------------------------------

    fn call_list(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let Some(target) = msg.param(1).filter(|t| !t.is_empty()) else {
            self.fail_call(id, "NEED_MORE_PARAMS", None, "Give a channel or nick", out);
            return;
        };
        // A call in a channel is only visible to that channel's members: its
        // participant list would otherwise disclose who is talking to whom.
        if self.config().is_valid_channel(target)
            && !self.channel(target).is_some_and(|c| c.contains(id))
        {
            self.fail_call(
                id,
                "NOT_IN_CHANNEL",
                None,
                "You are not on that channel",
                out,
            );
            return;
        }

        let folded = self.fold(target);
        if let Some(call) = self.calls().by_target(&folded) {
            let (call_id, media, count) = (call.id(), call.media(), call.len());
            let display = call.target().to_vec();
            let participants: Vec<ClientId> = call.accepted().collect();

            out.push(Action::Send {
                to: id,
                message: self
                    .call_reply(call_id, "INFO")
                    .param(display.clone())
                    .param(media.to_wire())
                    .param(count.to_string()),
            });
            for participant in participants {
                let Some(client) = self.client(participant) else {
                    continue;
                };
                let nick = client.nick_or_star().to_vec();
                let account = client
                    .account()
                    .map_or_else(|| b"*".to_vec(), <[u8]>::to_vec);
                out.push(Action::Send {
                    to: id,
                    message: self
                        .call_reply(call_id, "PARTICIPANT")
                        .param(display.clone())
                        .param(nick)
                        .param(account),
                });
            }
        }

        out.push(Action::Send {
            to: id,
            message: MessageBuf::new("CALL")
                .source(self.server_name())
                .param("*")
                .param("END")
                .param(target.to_vec())
                .trailing("End of call list"),
        });
    }

    // --- signalling --------------------------------------------------------

    fn call_signal(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let (Some(raw_id), Some(destination), Some(payload)) =
            (msg.param(1), msg.param(2), msg.param(3))
        else {
            self.fail_call(id, "NEED_MORE_PARAMS", None, "Not enough parameters", out);
            return;
        };
        // An id that does not resolve is reported the same way everywhere:
        // a client should not have to work out which subcommand it used to
        // learn that the call is gone.
        let Some(call_id) = CallId::parse(raw_id).filter(|c| self.calls().get(*c).is_some()) else {
            self.fail_call(id, "NO_SUCH_CALL", None, "No such call", out);
            return;
        };
        if payload.is_empty() || payload.len() > MAX_SIGNAL {
            self.fail_call(
                id,
                "INVALID_PAYLOAD",
                Some(call_id),
                "Malformed signalling payload",
                out,
            );
            return;
        }
        // Only participants may signal, and only to participants. Otherwise
        // the relay is an open channel to anybody whose nick you can guess.
        if !self
            .calls()
            .get(call_id)
            .is_some_and(|c| c.has_accepted(id))
        {
            self.fail_call(
                id,
                "NOT_IN_CALL",
                Some(call_id),
                "You are not in that call",
                out,
            );
            return;
        }

        let Some(mask) = self.client(id).map(Client::mask) else {
            return;
        };
        let relayed = MessageBuf::new("CALL")
            .source(mask)
            .param(call_id.to_wire())
            .param("SIGNAL")
            .trailing(payload.to_vec());

        if destination == b"*" {
            self.announce_to_call(call_id, &relayed, Some(id), out);
            return;
        }

        let Some(peer) = self.find_nick(destination) else {
            self.fail_call(id, "NO_SUCH_TARGET", Some(call_id), "No such nick", out);
            return;
        };
        if !self.calls().get(call_id).is_some_and(|c| c.contains(peer)) {
            self.fail_call(
                id,
                "NOT_IN_CALL",
                Some(call_id),
                "They are not in that call",
                out,
            );
            return;
        }
        out.push(Action::Send {
            to: peer,
            message: relayed,
        });
    }

    // --- helpers -----------------------------------------------------------

    /// Send a message to everyone who has accepted, optionally skipping one.
    fn announce_to_call(
        &self,
        call_id: CallId,
        message: &MessageBuf,
        except: Option<ClientId>,
        out: &mut Vec<Action>,
    ) {
        let Some(call) = self.calls().get(call_id) else {
            return;
        };
        for participant in call.accepted() {
            if Some(participant) != except {
                out.push(Action::Send {
                    to: participant,
                    message: message.clone(),
                });
            }
        }
    }

    fn call_reply(&self, call_id: CallId, verb: &str) -> MessageBuf {
        MessageBuf::new("CALL")
            .source(self.server_name())
            .param(call_id.to_wire())
            .param(verb)
    }

    /// Read a call id from parameter 1.
    fn call_id_param(
        &mut self,
        id: ClientId,
        msg: &Message<'_>,
        out: &mut Vec<Action>,
    ) -> Option<CallId> {
        let call_id = msg.param(1).and_then(CallId::parse);
        let Some(call_id) = call_id.filter(|c| self.calls().get(*c).is_some()) else {
            self.fail_call(id, "NO_SUCH_CALL", None, "No such call", out);
            return None;
        };
        Some(call_id)
    }

    /// Read a call id and a following parameter.
    fn call_and_param(
        &mut self,
        id: ClientId,
        msg: &Message<'_>,
        out: &mut Vec<Action>,
    ) -> Option<(CallId, Vec<u8>)> {
        let call_id = self.call_id_param(id, msg, out)?;
        let Some(param) = msg.param(2).filter(|p| !p.is_empty()) else {
            self.fail_call(
                id,
                "NEED_MORE_PARAMS",
                Some(call_id),
                "Not enough parameters",
                out,
            );
            return None;
        };
        Some((call_id, param.to_vec()))
    }

    fn fail_call(
        &self,
        id: ClientId,
        code: &str,
        call_id: Option<CallId>,
        text: &str,
        out: &mut Vec<Action>,
    ) {
        let mut message = MessageBuf::new("FAIL")
            .source(self.server_name())
            .param("CALL")
            .param(code.to_owned());
        if let Some(call_id) = call_id {
            message = message.param(call_id.to_wire());
        }
        out.push(Action::Send {
            to: id,
            message: message.trailing(text.to_owned()),
        });
    }
}
