//! `MODE`, `KICK` and `INVITE`.

use kestrel_proto::{Message, MessageBuf, numeric};

use crate::channel::BanEntry;
use crate::client::{Client, ClientId};
use crate::mask;
use crate::modes::{self, ModeChange};
use crate::server::{Action, Server};

impl Server {
    pub(crate) fn cmd_mode(
        &mut self,
        id: ClientId,
        msg: &Message<'_>,
        now: u64,
        out: &mut Vec<Action>,
    ) {
        let Some(target) = msg.param(0) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NEEDMOREPARAMS)
                    .param("MODE")
                    .trailing("Not enough parameters"),
            });
            return;
        };

        if self.config().is_valid_channel(target) {
            self.channel_mode(id, target, msg, now, out);
        } else {
            self.user_mode(id, target, out);
        }
    }

    /// `MODE <nick>`; only the client's own modes are visible.
    fn user_mode(&mut self, id: ClientId, target: &[u8], out: &mut Vec<Action>) {
        let is_self = self
            .client(id)
            .and_then(Client::nick)
            .is_some_and(|nick| self.config().casemapping.eq(nick, target));
        if is_self {
            // No user modes are implemented yet, so the set is always empty.
            out.push(Action::Send {
                to: id,
                message: MessageBuf::new("MODE")
                    .source(self.server_name())
                    .param(target.to_vec())
                    .param("+"),
            });
        } else {
            // Other people's modes are not disclosed, and neither is whether
            // the nickname exists.
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NOSUCHNICK)
                    .param(target.to_vec())
                    .trailing("Cannot change mode for other users"),
            });
        }
    }

    fn channel_mode(
        &mut self,
        id: ClientId,
        name: &[u8],
        msg: &Message<'_>,
        now: u64,
        out: &mut Vec<Action>,
    ) {
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

        // With no mode string this is a query.
        let Some(spec) = msg.param(1) else {
            let (flags, params) = channel.modes().render();
            let created = channel.created_at();
            let mut reply = self
                .numeric(id, numeric::RPL_CHANNELMODEIS)
                .param(display.clone())
                .param(flags);
            for param in params {
                reply = reply.param(param);
            }
            out.push(Action::Send {
                to: id,
                message: reply,
            });
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::RPL_CREATIONTIME)
                    .param(display)
                    .param(created.to_string()),
            });
            return;
        };

        // `MODE #chan b` with no mask is a request to read the ban list, which
        // any member may do.
        if (spec == b"+b" || spec == b"b") && msg.param(2).is_none() {
            self.send_ban_list(id, &display, out);
            return;
        }

        let params: Vec<&[u8]> = msg.params()[2.min(msg.params().len())..].to_vec();
        let parsed = modes::parse(spec, &params, modes::channel_mode_kind);

        for letter in &parsed.unknown {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_UNKNOWNMODE)
                    .param(vec![*letter])
                    .trailing("is an unknown mode char to me"),
            });
        }
        if parsed.changes.is_empty() {
            return;
        }

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
        if !channel.status(id).is_some_and(|s| s.operator) {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_CHANOPRIVSNEEDED)
                    .param(display)
                    .trailing("You're not channel operator"),
            });
            return;
        }

        let Some(mask_of_setter) = self.client(id).map(Client::mask) else {
            return;
        };
        let folded = self.fold(name);
        let mut applied: Vec<ModeChange> = Vec::new();

        for change in parsed.changes {
            if self.apply_channel_mode(id, &folded, &change, &mask_of_setter, now, out) {
                applied.push(change);
            }
        }

        if applied.is_empty() {
            return;
        }
        let (spec, params) = modes::render(&applied);
        let mut announcement = MessageBuf::new("MODE")
            .source(mask_of_setter)
            .param(display)
            .param(spec);
        for param in params {
            announcement = announcement.param(param);
        }
        self.broadcast_channel(out, &folded, &announcement, None);
    }

    /// Apply one change, returning whether it actually altered anything.
    ///
    /// Changes that are already in effect are reported as not applied, so the
    /// broadcast only ever describes real state transitions.
    fn apply_channel_mode(
        &mut self,
        actor: ClientId,
        folded: &[u8],
        change: &ModeChange,
        setter_mask: &[u8],
        now: u64,
        out: &mut Vec<Action>,
    ) -> bool {
        match change.letter {
            b'o' | b'v' => {
                let Some(nick) = change.param.as_deref() else {
                    return false;
                };
                let Some(target) = self.find_nick(nick) else {
                    out.push(Action::Send {
                        to: actor,
                        message: self
                            .numeric(actor, numeric::ERR_NOSUCHNICK)
                            .param(nick.to_vec())
                            .trailing("No such nick/channel"),
                    });
                    return false;
                };
                let display = self
                    .channel_by_folded(folded)
                    .map_or_else(Vec::new, |c| c.name().to_vec());
                if !self
                    .channel_by_folded(folded)
                    .is_some_and(|c| c.contains(target))
                {
                    out.push(Action::Send {
                        to: actor,
                        message: self
                            .numeric(actor, numeric::ERR_USERNOTINCHANNEL)
                            .param(nick.to_vec())
                            .param(display)
                            .trailing("They aren't on that channel"),
                    });
                    return false;
                }
                self.set_member_status(folded, target, change.letter, change.adding)
            }
            b'b' => {
                let Some(raw) = change.param.as_deref() else {
                    return false;
                };
                let normalised = mask::normalise(raw);
                self.set_ban(folded, &normalised, setter_mask, now, change.adding)
            }
            b'k' => {
                let key = if change.adding {
                    change.param.clone()
                } else {
                    None
                };
                self.set_channel_key(folded, key, change.adding)
            }
            b'l' => {
                let limit = if change.adding {
                    match change
                        .param
                        .as_deref()
                        .and_then(|p| std::str::from_utf8(p).ok())
                        .and_then(|p| p.parse::<usize>().ok())
                        .filter(|n| *n > 0)
                    {
                        Some(n) => Some(n),
                        // A non-numeric or zero limit is meaningless; ignore
                        // it rather than wedging the channel shut.
                        None => return false,
                    }
                } else {
                    None
                };
                self.set_channel_limit(folded, limit)
            }
            flag @ (b'i' | b'm' | b'n' | b's' | b't') => {
                self.set_channel_flag(folded, flag, change.adding)
            }
            _ => false,
        }
    }

    fn send_ban_list(&self, id: ClientId, display: &[u8], out: &mut Vec<Action>) {
        if let Some(channel) = self.channel(display) {
            for ban in channel.bans() {
                out.push(Action::Send {
                    to: id,
                    message: self
                        .numeric(id, numeric::RPL_BANLIST)
                        .param(display.to_vec())
                        .param(ban.mask.clone())
                        .param(ban.setter.clone())
                        .param(ban.set_at.to_string()),
                });
            }
        }
        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_ENDOFBANLIST)
                .param(display.to_vec())
                .trailing("End of channel ban list"),
        });
    }

    pub(crate) fn cmd_kick(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let (Some(name), Some(target_nick)) = (msg.param(0), msg.param(1)) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NEEDMOREPARAMS)
                    .param("KICK")
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
        if !channel.status(id).is_some_and(|s| s.operator) {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_CHANOPRIVSNEEDED)
                    .param(display)
                    .trailing("You're not channel operator"),
            });
            return;
        }

        let Some(target) = self.find_nick(target_nick).filter(|t| channel.contains(*t)) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_USERNOTINCHANNEL)
                    .param(target_nick.to_vec())
                    .param(display)
                    .trailing("They aren't on that channel"),
            });
            return;
        };

        let reason = msg
            .param(2)
            .map_or_else(|| target_nick.to_vec(), |r| self.truncate_reason(r));
        let Some(actor_mask) = self.client(id).map(Client::mask) else {
            return;
        };
        let target_display = self
            .client(target)
            .and_then(Client::nick)
            .map_or_else(Vec::new, <[u8]>::to_vec);
        let folded = self.fold(name);

        let kick = MessageBuf::new("KICK")
            .source(actor_mask)
            .param(display)
            .param(target_display)
            .trailing(reason);
        // Broadcast before removing, so the kicked client learns why.
        self.broadcast_channel(out, &folded, &kick, None);
        self.remove_member(&folded, target);
    }

    pub(crate) fn cmd_invite(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let (Some(target_nick), Some(name)) = (msg.param(0), msg.param(1)) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NEEDMOREPARAMS)
                    .param("INVITE")
                    .trailing("Not enough parameters"),
            });
            return;
        };

        let Some(target) = self.find_nick(target_nick) else {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_NOSUCHNICK)
                    .param(target_nick.to_vec())
                    .trailing("No such nick/channel"),
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
        if channel.contains(target) {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_USERONCHANNEL)
                    .param(target_nick.to_vec())
                    .param(display)
                    .trailing("is already on channel"),
            });
            return;
        }
        // Only operators may invite past +i; otherwise any member could let
        // anyone into a channel that was deliberately closed.
        if channel.modes().invite_only && !channel.status(id).is_some_and(|s| s.operator) {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_CHANOPRIVSNEEDED)
                    .param(display)
                    .trailing("You're not channel operator"),
            });
            return;
        }

        let folded = self.fold(name);
        self.record_invite(&folded, target);

        let Some(actor_mask) = self.client(id).map(Client::mask) else {
            return;
        };
        let target_display = self
            .client(target)
            .and_then(Client::nick)
            .map_or_else(Vec::new, <[u8]>::to_vec);

        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_INVITING)
                .param(target_display.clone())
                .param(display.clone()),
        });
        out.push(Action::Send {
            to: target,
            message: MessageBuf::new("INVITE")
                .source(actor_mask)
                .param(target_display.clone())
                .param(display),
        });
        self.announce_invite(&folded, id, &target_display, out);
    }
}

/// Build a ban entry.
pub(crate) fn ban_entry(mask: &[u8], setter: &[u8], set_at: u64) -> BanEntry {
    BanEntry {
        mask: mask.to_vec(),
        setter: setter.to_vec(),
        set_at,
    }
}
