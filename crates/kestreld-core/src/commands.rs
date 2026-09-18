//! Channel and messaging commands.

use kestrel_proto::{Message, MessageBuf, numeric};

use crate::channel::{Channel, Topic};
use crate::client::{Client, ClientId};
use crate::server::{Action, Server};

/// Split a comma-separated parameter, as `JOIN`, `PART` and `PRIVMSG` use.
fn split_list(param: &[u8]) -> Vec<&[u8]> {
    param
        .split(|&b| b == b',')
        .filter(|p| !p.is_empty())
        .collect()
}

impl Server {
    pub(crate) fn cmd_ping(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let token = msg.param(0).unwrap_or(b"").to_vec();
        out.push(Action::Send {
            to: id,
            message: MessageBuf::new("PONG")
                .source(self.server_name())
                .param(self.server_name())
                .trailing(token),
        });
    }

    pub(crate) fn cmd_quit(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let reason = msg.param(0).unwrap_or(b"Client Quit");
        let reason = self.truncate_reason(reason);
        let mut text = b"Quit: ".to_vec();
        text.extend_from_slice(&reason);
        self.quit(id, &text, out);
    }

    pub(crate) fn cmd_away(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        if let Some(message) = msg.param(0).filter(|m| !m.is_empty()) {
            let message = self.truncate_reason(message);
            self.set_away(id, Some(message));
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::RPL_NOWAWAY)
                    .trailing("You have been marked as being away"),
            });
        } else {
            // AWAY with no message, or an empty one, clears away status.
            self.set_away(id, None);
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::RPL_UNAWAY)
                    .trailing("You are no longer marked as being away"),
            });
        }
    }

    pub(crate) fn cmd_join(
        &mut self,
        id: ClientId,
        msg: &Message<'_>,
        now: u64,
        out: &mut Vec<Action>,
    ) {
        let Some(list) = msg.param(0) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NEEDMOREPARAMS)
                    .param("JOIN")
                    .trailing("Not enough parameters"),
            });
            return;
        };

        let names: Vec<Vec<u8>> = split_list(list).into_iter().map(<[u8]>::to_vec).collect();
        let keys: Vec<Vec<u8>> = msg
            .param(1)
            .map(|k| split_list(k).into_iter().map(<[u8]>::to_vec).collect())
            .unwrap_or_default();

        for (index, name) in names.iter().enumerate() {
            self.join_one(id, name, keys.get(index).map(Vec::as_slice), now, out);
        }
    }

    fn join_one(
        &mut self,
        id: ClientId,
        name: &[u8],
        key: Option<&[u8]>,
        now: u64,
        out: &mut Vec<Action>,
    ) {
        if !self.config().is_valid_channel(name) {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NOSUCHCHANNEL)
                    .param(name.to_vec())
                    .trailing("No such channel"),
            });
            return;
        }

        let folded = self.fold(name);

        if self.channel(name).is_some_and(|c| c.contains(id)) {
            return; // Already in it; joining again is a no-op.
        }

        let in_count = self.client(id).map_or(0, |c| c.channels().len());
        if in_count >= self.config().max_channels_per_client {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_TOOMANYCHANNELS)
                    .param(name.to_vec())
                    .trailing("You have joined too many channels"),
            });
            return;
        }

        if let Some(denial) = self.join_denial(id, name, key) {
            out.push(Action::Send {
                to: id,
                message: denial,
            });
            return;
        }

        self.insert_member(&folded, name.to_vec(), id, now);

        let Some(mask) = self.client(id).map(Client::mask) else {
            return;
        };
        let display = self
            .channel(name)
            .map_or_else(|| name.to_vec(), |c| c.name().to_vec());

        // Everyone in the channel, including the joiner, sees the JOIN first.
        let join = MessageBuf::new("JOIN").source(mask).param(display.clone());
        self.broadcast_channel(out, &folded, &join, None);

        // Then the joiner alone gets the channel's current state.
        if let Some(topic) = self.channel(name).and_then(Channel::topic) {
            let (text, setter, set_at) = (topic.text.clone(), topic.setter.clone(), topic.set_at);
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::RPL_TOPIC)
                    .param(display.clone())
                    .trailing(text),
            });
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::RPL_TOPICWHOTIME)
                    .param(display.clone())
                    .param(setter)
                    .param(set_at.to_string()),
            });
        }
        self.send_names(id, &display, out);
    }

    /// Why `id` may not join `name`, if it may not.
    ///
    /// Entry conditions apply only to channels that already exist: a channel
    /// you are creating cannot be full, keyed, banned or invite-only.
    fn join_denial(&self, id: ClientId, name: &[u8], key: Option<&[u8]>) -> Option<MessageBuf> {
        let channel = self.channel(name)?;
        let modes = channel.modes();

        if let Some(limit) = modes.limit
            && channel.len() >= limit
        {
            return Some(
                self.numeric(id, numeric::ERR_CHANNELISFULL)
                    .param(name.to_vec())
                    .trailing("Cannot join channel (+l)"),
            );
        }
        if let Some(expected) = modes.key.as_deref()
            && key != Some(expected)
        {
            return Some(
                self.numeric(id, numeric::ERR_BADCHANNELKEY)
                    .param(name.to_vec())
                    .trailing("Cannot join channel (+k)"),
            );
        }
        // A ban keeps you out unless you were explicitly invited past it.
        let joiner_mask = self.client(id).map(Client::mask).unwrap_or_default();
        if channel.is_banned(&joiner_mask) && !channel.is_invited(id) {
            return Some(
                self.numeric(id, numeric::ERR_BANNEDFROMCHAN)
                    .param(name.to_vec())
                    .trailing("Cannot join channel (+b)"),
            );
        }
        if modes.invite_only && !channel.is_invited(id) {
            return Some(
                self.numeric(id, numeric::ERR_INVITEONLYCHAN)
                    .param(name.to_vec())
                    .trailing("Cannot join channel (+i)"),
            );
        }
        None
    }

    pub(crate) fn cmd_part(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let Some(list) = msg.param(0) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NEEDMOREPARAMS)
                    .param("PART")
                    .trailing("Not enough parameters"),
            });
            return;
        };
        let reason = msg
            .param(1)
            .map(|r| self.truncate_reason(r))
            .unwrap_or_default();
        let names: Vec<Vec<u8>> = split_list(list).into_iter().map(<[u8]>::to_vec).collect();

        for name in names {
            let Some(channel) = self.channel(&name) else {
                out.push(Action::Send {
                    to: id,
                    message: self
                        .numeric(id, numeric::ERR_NOSUCHCHANNEL)
                        .param(name.clone())
                        .trailing("No such channel"),
                });
                continue;
            };
            if !channel.contains(id) {
                out.push(Action::Send {
                    to: id,
                    message: self
                        .numeric(id, numeric::ERR_NOTONCHANNEL)
                        .param(name.clone())
                        .trailing("You're not on that channel"),
                });
                continue;
            }

            let display = channel.name().to_vec();
            let folded = self.fold(&name);
            let Some(mask) = self.client(id).map(Client::mask) else {
                continue;
            };

            let mut part = MessageBuf::new("PART").source(mask).param(display);
            if !reason.is_empty() {
                part = part.trailing(reason.clone());
            }
            // Broadcast before removing, so the leaver also sees their own PART.
            self.broadcast_channel(out, &folded, &part, None);
            self.remove_member(&folded, id);
        }
    }

    pub(crate) fn cmd_privmsg(
        &mut self,
        id: ClientId,
        msg: &Message<'_>,
        is_notice: bool,
        out: &mut Vec<Action>,
    ) {
        // A NOTICE must never produce an automatic reply. The rule exists so
        // that two servers or bots cannot bounce errors off each other
        // forever, and it applies to error numerics too.
        let target = msg.param(0).filter(|t| !t.is_empty());
        let text = msg.param(1).filter(|t| !t.is_empty());

        let (Some(target), Some(text)) = (target, text) else {
            if !is_notice {
                let code = if target.is_none() {
                    numeric::ERR_NORECIPIENT
                } else {
                    numeric::ERR_NOTEXTTOSEND
                };
                out.push(Action::Send {
                    to: id,
                    message: self.numeric(id, code).trailing("No text to send"),
                });
            }
            return;
        };

        let command = if is_notice { "NOTICE" } else { "PRIVMSG" };
        let Some(mask) = self.client(id).map(Client::mask) else {
            return;
        };

        if self.config().is_valid_channel(target) {
            self.message_channel(id, target, command, text, &mask, is_notice, out);
        } else {
            self.message_user(id, target, command, text, &mask, is_notice, out);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn message_channel(
        &mut self,
        id: ClientId,
        target: &[u8],
        command: &str,
        text: &[u8],
        mask: &[u8],
        is_notice: bool,
        out: &mut Vec<Action>,
    ) {
        let Some(channel) = self.channel(target) else {
            if !is_notice {
                out.push(Action::Send {
                    to: id,
                    message: self
                        .numeric(id, numeric::ERR_NOSUCHCHANNEL)
                        .param(target.to_vec())
                        .trailing("No such channel"),
                });
            }
            return;
        };

        let is_member = channel.contains(id);
        let modes = channel.modes();
        let may_speak_over_restrictions = channel
            .status(id)
            .is_some_and(crate::channel::MemberStatus::may_speak_when_moderated);
        // Being banned silences you even while still in the channel, unless an
        // operator has voiced you — which is the usual way a ban is softened.
        let banned = !may_speak_over_restrictions
            && channel.is_banned(&self.client(id).map(Client::mask).unwrap_or_default());
        let blocked = (modes.no_external_messages && !is_member)
            || (modes.moderated && !may_speak_over_restrictions)
            || banned;

        if blocked {
            if !is_notice {
                out.push(Action::Send {
                    to: id,
                    message: self
                        .numeric(id, numeric::ERR_CANNOTSENDTOCHAN)
                        .param(channel.name().to_vec())
                        .trailing("Cannot send to channel"),
                });
            }
            return;
        }

        let display = channel.name().to_vec();
        let folded = self.fold(target);
        let message = MessageBuf::new(command)
            .source(mask.to_vec())
            .param(display)
            .trailing(text.to_vec());
        // The sender does not receive an echo of their own message unless they
        // negotiated echo-message, which nothing does yet.
        self.broadcast_channel(out, &folded, &message, Some(id));
    }

    #[allow(clippy::too_many_arguments)]
    fn message_user(
        &mut self,
        id: ClientId,
        target: &[u8],
        command: &str,
        text: &[u8],
        mask: &[u8],
        is_notice: bool,
        out: &mut Vec<Action>,
    ) {
        let Some(recipient) = self.find_nick(target) else {
            if !is_notice {
                out.push(Action::Send {
                    to: id,
                    message: self
                        .numeric(id, numeric::ERR_NOSUCHNICK)
                        .param(target.to_vec())
                        .trailing("No such nick/channel"),
                });
            }
            return;
        };

        let Some(nick) = self
            .client(recipient)
            .and_then(|c| c.nick())
            .map(<[u8]>::to_vec)
        else {
            return;
        };

        out.push(Action::Send {
            to: recipient,
            message: MessageBuf::new(command)
                .source(mask.to_vec())
                .param(nick.clone())
                .trailing(text.to_vec()),
        });

        // Tell the sender if the recipient is away, but never in reply to a
        // NOTICE.
        if !is_notice
            && let Some(away) = self
                .client(recipient)
                .and_then(Client::away)
                .map(<[u8]>::to_vec)
        {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::RPL_AWAY)
                    .param(nick)
                    .trailing(away),
            });
        }
    }

    pub(crate) fn cmd_topic(
        &mut self,
        id: ClientId,
        msg: &Message<'_>,
        now: u64,
        out: &mut Vec<Action>,
    ) {
        let Some(name) = msg.param(0) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NEEDMOREPARAMS)
                    .param("TOPIC")
                    .trailing("Not enough parameters"),
            });
            return;
        };

        let Some(channel) = self.channel(name) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NOSUCHCHANNEL)
                    .param(name.to_vec())
                    .trailing("No such channel"),
            });
            return;
        };
        let display = channel.name().to_vec();

        // With no second parameter this is a query, not a change.
        let Some(new_topic) = msg.param(1) else {
            match channel.topic() {
                Some(topic) => {
                    let (text, setter, set_at) =
                        (topic.text.clone(), topic.setter.clone(), topic.set_at);
                    out.push(Action::Send {
                        to: id,
                        message: self
                            .numeric(id, numeric::RPL_TOPIC)
                            .param(display.clone())
                            .trailing(text),
                    });
                    out.push(Action::Send {
                        to: id,
                        message: self
                            .numeric(id, numeric::RPL_TOPICWHOTIME)
                            .param(display)
                            .param(setter)
                            .param(set_at.to_string()),
                    });
                }
                None => out.push(Action::Send {
                    to: id,
                    message: self
                        .numeric(id, numeric::RPL_NOTOPIC)
                        .param(display)
                        .trailing("No topic is set"),
                }),
            }
            return;
        };

        if !channel.contains(id) {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NOTONCHANNEL)
                    .param(display)
                    .trailing("You're not on that channel"),
            });
            return;
        }

        let is_op = channel.status(id).is_some_and(|status| status.operator);
        if channel.modes().topic_protected && !is_op {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_CHANOPRIVSNEEDED)
                    .param(display)
                    .trailing("You're not channel operator"),
            });
            return;
        }

        let mut text = new_topic.to_vec();
        text.truncate(self.config().max_topic_len);
        let Some(mask) = self.client(id).map(Client::mask) else {
            return;
        };
        let folded = self.fold(name);

        self.set_topic(
            &folded,
            if text.is_empty() {
                None
            } else {
                Some(Topic {
                    text: text.clone(),
                    setter: mask.clone(),
                    set_at: now,
                })
            },
        );

        let announcement = MessageBuf::new("TOPIC")
            .source(mask)
            .param(display)
            .trailing(text);
        self.broadcast_channel(out, &folded, &announcement, None);
    }

    pub(crate) fn cmd_names(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let Some(list) = msg.param(0) else {
            // NAMES with no argument would list every channel on the network,
            // which is expensive and rarely what anyone wants; treat it as an
            // empty listing rather than a scan.
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::RPL_ENDOFNAMES)
                    .param("*")
                    .trailing("End of /NAMES list"),
            });
            return;
        };
        let names: Vec<Vec<u8>> = split_list(list).into_iter().map(<[u8]>::to_vec).collect();
        for name in names {
            let display = self
                .channel(&name)
                .map_or_else(|| name.clone(), |c| c.name().to_vec());
            self.send_names(id, &display, out);
        }
    }

    /// Send the member list for one channel.
    pub(crate) fn send_names(&self, id: ClientId, name: &[u8], out: &mut Vec<Action>) {
        if let Some(channel) = self.channel(name) {
            let mut entries: Vec<Vec<u8>> = channel
                .member_ids()
                .filter_map(|member| {
                    let nick = self.client(member)?.nick()?;
                    let mut entry = channel.status(member)?.prefixes();
                    // Only the highest prefix, until multi-prefix is offered.
                    entry.truncate(1);
                    entry.extend_from_slice(nick);
                    Some(entry)
                })
                .collect();
            // Stable output keeps tests deterministic and clients tidy.
            entries.sort_unstable();

            // `=` means a public channel, as opposed to `*` secret or `@` private.
            let visibility = if channel.modes().secret { "@" } else { "=" };

            for chunk in entries.chunks(12) {
                let joined = chunk.join(&b' ');
                out.push(Action::Send {
                    to: id,
                    message: self
                        .numeric(id, numeric::RPL_NAMREPLY)
                        .param(visibility)
                        .param(channel.name().to_vec())
                        .trailing(joined),
                });
            }
        }

        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_ENDOFNAMES)
                .param(name.to_vec())
                .trailing("End of /NAMES list"),
        });
    }

    pub(crate) fn cmd_whois(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let Some(target) = msg.param(0).filter(|t| !t.is_empty()) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NONICKNAMEGIVEN)
                    .trailing("No nickname given"),
            });
            return;
        };

        let Some(subject_id) = self.find_nick(target) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NOSUCHNICK)
                    .param(target.to_vec())
                    .trailing("No such nick/channel"),
            });
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::RPL_ENDOFWHOIS)
                    .param(target.to_vec())
                    .trailing("End of /WHOIS list"),
            });
            return;
        };

        let Some(subject) = self.client(subject_id) else {
            return;
        };
        let nick = subject.nick_or_star().to_vec();
        let user = subject.user().unwrap_or(b"*").to_vec();
        let host = subject.host().to_vec();
        let realname = subject.realname().to_vec();
        let away = subject.away().map(<[u8]>::to_vec);

        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_WHOISUSER)
                .param(nick.clone())
                .param(user)
                .param(host)
                .param("*")
                .trailing(realname),
        });

        // Channels the subject shares, so a WHOIS does not reveal channels the
        // asker cannot see.
        let mut shared: Vec<Vec<u8>> = Vec::new();
        for folded in subject.channels() {
            if let Some(channel) = self.channel_by_folded(folded) {
                let mut entry = channel
                    .status(subject_id)
                    .map(crate::channel::MemberStatus::prefixes)
                    .unwrap_or_default();
                entry.truncate(1);
                entry.extend_from_slice(channel.name());
                shared.push(entry);
            }
        }
        shared.sort_unstable();
        if !shared.is_empty() {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::RPL_WHOISCHANNELS)
                    .param(nick.clone())
                    .trailing(shared.join(&b' ')),
            });
        }

        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_WHOISSERVER)
                .param(nick.clone())
                .param(self.server_name())
                .trailing(String::from_utf8_lossy(&self.config().network_name).into_owned()),
        });

        if let Some(away) = away {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::RPL_AWAY)
                    .param(nick.clone())
                    .trailing(away),
            });
        }

        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_ENDOFWHOIS)
                .param(nick)
                .trailing("End of /WHOIS list"),
        });
    }
}
