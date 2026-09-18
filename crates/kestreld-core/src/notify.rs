//! `TAGMSG` and `SETNAME`, and the notifications that go with capabilities.

use kestrel_proto::{Message, MessageBuf, numeric};

use crate::client::{Client, ClientId};
use crate::server::{Action, Server};

impl Server {
    /// `TAGMSG <target>` — a message carrying only tags.
    ///
    /// This is what makes client-only tags useful on their own: a typing
    /// notification or a reaction is metadata with no text, and sending it as
    /// an empty `PRIVMSG` would show up as a blank line on every client that
    /// does not understand it.
    pub(crate) fn cmd_tagmsg(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let Some(target) = msg.param(0).filter(|t| !t.is_empty()) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NORECIPIENT)
                    .trailing("No recipient given (TAGMSG)"),
            });
            return;
        };

        let tags: Vec<(Vec<u8>, Vec<u8>)> = msg
            .tags()
            .iter()
            .filter(|tag| tag.is_client_only())
            .map(|tag| (tag.key.to_vec(), tag.raw_value.to_vec()))
            .collect();
        if tags.is_empty() {
            return; // Nothing to relay.
        }

        let Some(mask) = self.client(id).map(Client::mask) else {
            return;
        };

        let build = |target: Vec<u8>| {
            let mut message = MessageBuf::new("TAGMSG").source(mask.clone()).param(target);
            for (key, raw_value) in &tags {
                message = message.raw_tag(key.clone(), raw_value.clone());
            }
            message
        };

        if self.config().is_valid_channel(target) {
            let Some(channel) = self.channel(target) else {
                return; // A TAGMSG to nowhere is silently dropped, like a NOTICE.
            };
            if channel.modes().no_external_messages && !channel.contains(id) {
                return;
            }
            let display = channel.name().to_vec();
            let folded = self.fold(target);
            let echo = self.client(id).is_some_and(|c| c.has_cap("echo-message"));
            let except = if echo { None } else { Some(id) };

            // The specification is explicit that TAGMSG reaches only clients
            // that negotiated message-tags. That is what keeps it invisible to
            // everyone else rather than showing up as noise.
            self.broadcast_channel_with(out, &folded, except, |member| {
                member
                    .has_cap("message-tags")
                    .then(|| build(display.clone()))
            });
        } else if let Some(recipient) = self.find_nick(target)
            && self
                .client(recipient)
                .is_some_and(|c| c.has_cap("message-tags"))
            && let Some(nick) = self.client(recipient).and_then(Client::nick)
        {
            out.push(Action::Send {
                to: recipient,
                message: build(nick.to_vec()),
            });
        }
    }

    /// `SETNAME :<realname>` — change the realname without reconnecting.
    pub(crate) fn cmd_setname(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let Some(realname) = msg.param(0).filter(|r| !r.is_empty()) else {
            out.push(Action::Send {
                to: id,
                message: MessageBuf::new("FAIL")
                    .source(self.server_name())
                    .param("SETNAME")
                    .param("INVALID_REALNAME")
                    .trailing("Realname cannot be empty"),
            });
            return;
        };

        let mut realname = realname.to_vec();
        realname.truncate(400);
        self.set_realname(id, realname.clone());

        let Some(mask) = self.client(id).map(Client::mask) else {
            return;
        };
        let announcement = MessageBuf::new("SETNAME").source(mask).trailing(realname);
        // The client that made the change always sees it confirmed, whether or
        // not it negotiated the capability.
        out.push(Action::Send {
            to: id,
            message: announcement.clone(),
        });
        self.notify_peers_with_cap(out, id, "setname", &announcement, false);
    }

    /// Tell peers that a client logged in to, or out of, an account.
    pub(crate) fn announce_account(&self, id: ClientId, out: &mut Vec<Action>) {
        let Some(client) = self.client(id) else {
            return;
        };
        let account = client
            .account()
            .map_or_else(|| b"*".to_vec(), <[u8]>::to_vec);
        let announcement = MessageBuf::new("ACCOUNT")
            .source(client.mask())
            .param(account);
        self.notify_peers_with_cap(out, id, "account-notify", &announcement, false);
    }

    /// Tell channel operators that someone was invited.
    ///
    /// Operators are the ones who can act on it; telling every member would
    /// turn an invitation into an announcement, which is not what it is.
    pub(crate) fn announce_invite(
        &self,
        folded_channel: &[u8],
        inviter: ClientId,
        invited_nick: &[u8],
        out: &mut Vec<Action>,
    ) {
        let Some(channel) = self.channel_by_folded(folded_channel) else {
            return;
        };
        let Some(mask) = self.client(inviter).map(Client::mask) else {
            return;
        };
        let display = channel.name().to_vec();

        for member in channel.member_ids() {
            if member == inviter {
                continue;
            }
            let is_operator = channel.status(member).is_some_and(|s| s.operator);
            let wants = self
                .client(member)
                .is_some_and(|c| c.has_cap("invite-notify"));
            if is_operator && wants {
                out.push(Action::Send {
                    to: member,
                    message: MessageBuf::new("INVITE")
                        .source(mask.clone())
                        .param(invited_nick.to_vec())
                        .param(display.clone()),
                });
            }
        }
    }
}
