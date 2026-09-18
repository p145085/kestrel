//! `WHO`, `LIST`, `ISON` and `USERHOST`.
//!
//! These are the commands that disclose information about other users, so each
//! one has to decide what the asker is allowed to see. The rule throughout is
//! that a secret channel is invisible to anyone not in it, including in the
//! indirect ways — a `WHO` that names it, a `LIST` that counts it.

use kestrel_proto::{Message, MessageBuf, numeric};

use crate::client::{Client, ClientId};
use crate::mask;
use crate::server::{Action, Server};

impl Server {
    pub(crate) fn cmd_who(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let target = msg.param(0).unwrap_or(b"*").to_vec();

        if let Some(channel) = self.channel(&target) {
            let visible = !channel.modes().secret || channel.contains(id);
            if visible {
                let display = channel.name().to_vec();
                let members: Vec<ClientId> = channel.member_ids().collect();
                let mut replies: Vec<MessageBuf> = members
                    .into_iter()
                    .filter_map(|member| self.who_reply(id, member, Some(&display)))
                    .collect();
                // Deterministic ordering keeps output stable for clients and
                // tests alike; HashMap iteration order is not.
                replies.sort_by_cached_key(|r| r.to_vec().unwrap_or_default());
                for message in replies {
                    out.push(Action::Send { to: id, message });
                }
            }
        } else {
            // Otherwise treat the parameter as a nick or a mask over masks.
            let pattern = mask::normalise(&target);
            let matching: Vec<ClientId> = self
                .clients_iter()
                .filter(|(_, client)| {
                    client.is_registered() && mask::matches(&pattern, &client.mask())
                })
                .map(|(cid, _)| cid)
                .collect();
            let mut replies: Vec<MessageBuf> = matching
                .into_iter()
                .filter_map(|member| self.who_reply(id, member, None))
                .collect();
            replies.sort_by_cached_key(|r| r.to_vec().unwrap_or_default());
            for message in replies {
                out.push(Action::Send { to: id, message });
            }
        }

        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_ENDOFWHO)
                .param(target)
                .trailing("End of /WHO list"),
        });
    }

    /// Build one `RPL_WHOREPLY` line.
    ///
    /// `channel` names the channel being listed, or `None` for a bare nick
    /// query, in which case a channel the subject is in is chosen — but only
    /// one the asker is allowed to see.
    fn who_reply(
        &self,
        asker: ClientId,
        subject_id: ClientId,
        channel: Option<&[u8]>,
    ) -> Option<MessageBuf> {
        let subject = self.client(subject_id)?;
        let nick = subject.nick()?.to_vec();

        let shown_channel = match channel {
            Some(name) => name.to_vec(),
            None => self
                .visible_channel_of(asker, subject_id)
                .unwrap_or_else(|| b"*".to_vec()),
        };

        // `H` here, `G` gone. The prefix, if any, follows.
        let mut flags = vec![if subject.away().is_some() { b'G' } else { b'H' }];
        if let Some(status) = self
            .channel(&shown_channel)
            .and_then(|c| c.status(subject_id))
            && let Some(prefix) = status.prefix()
        {
            flags.push(prefix);
        }

        let mut realname = b"0 ".to_vec();
        realname.extend_from_slice(subject.realname());

        Some(
            self.numeric(asker, numeric::RPL_WHOREPLY)
                .param(shown_channel)
                .param(subject.user().unwrap_or(b"*").to_vec())
                .param(subject.host().to_vec())
                .param(self.server_name())
                .param(nick)
                .param(flags)
                .trailing(realname),
        )
    }

    /// A channel shared by the subject that the asker is permitted to see.
    fn visible_channel_of(&self, asker: ClientId, subject: ClientId) -> Option<Vec<u8>> {
        let mut candidates: Vec<Vec<u8>> = self
            .client(subject)?
            .channels()
            .iter()
            .filter_map(|folded| {
                let channel = self.channel_by_folded(folded)?;
                // A secret channel is disclosed only to its own members.
                (!channel.modes().secret || channel.contains(asker))
                    .then(|| channel.name().to_vec())
            })
            .collect();
        candidates.sort_unstable();
        candidates.into_iter().next()
    }

    pub(crate) fn cmd_list(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_LISTSTART)
                .param("Channel")
                .trailing("Users  Name"),
        });

        let requested: Option<Vec<Vec<u8>>> = msg.param(0).map(|list| {
            list.split(|&b| b == b',')
                .filter(|n| !n.is_empty())
                .map(<[u8]>::to_vec)
                .collect()
        });

        let mut rows: Vec<(Vec<u8>, usize, Vec<u8>)> = self
            .channels_iter()
            .filter(|(_, channel)| {
                // Secret channels are omitted entirely from listings.
                if channel.modes().secret && !channel.contains(id) {
                    return false;
                }
                requested.as_ref().is_none_or(|names| {
                    names
                        .iter()
                        .any(|n| self.config().casemapping.eq(n, channel.name()))
                })
            })
            .map(|(_, channel)| {
                (
                    channel.name().to_vec(),
                    channel.len(),
                    channel.topic().map(|t| t.text.clone()).unwrap_or_default(),
                )
            })
            .collect();
        rows.sort_unstable();

        for (name, count, topic) in rows {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::RPL_LIST)
                    .param(name)
                    .param(count.to_string())
                    .trailing(topic),
            });
        }

        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_LISTEND)
                .trailing("End of /LIST"),
        });
    }

    pub(crate) fn cmd_ison(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        // ISON takes its nicknames as separate parameters, but clients
        // routinely send them space-separated in a trailing parameter too.
        let online: Vec<Vec<u8>> = msg
            .params()
            .iter()
            .flat_map(|param| param.split(|&b| b == b' '))
            .filter(|nick| !nick.is_empty())
            .filter_map(|nick| {
                let found = self.find_nick(nick)?;
                self.client(found)?.nick().map(<[u8]>::to_vec)
            })
            .collect();

        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_ISON)
                .trailing(online.join(&b' ')),
        });
    }

    pub(crate) fn cmd_userhost(&mut self, id: ClientId, msg: &Message<'_>, out: &mut Vec<Action>) {
        let replies: Vec<Vec<u8>> = msg
            .params()
            .iter()
            .flat_map(|param| param.split(|&b| b == b' '))
            .filter(|nick| !nick.is_empty())
            // Five is the traditional cap, and keeps the reply inside a line.
            .take(5)
            .filter_map(|nick| {
                let found = self.find_nick(nick)?;
                let client = self.client(found)?;
                let mut reply = client.nick()?.to_vec();
                reply.push(b'=');
                reply.push(if client.away().is_some() { b'-' } else { b'+' });
                reply.extend_from_slice(client.user().unwrap_or(b"*"));
                reply.push(b'@');
                reply.extend_from_slice(client.host());
                Some(reply)
            })
            .collect();

        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_USERHOST)
                .trailing(replies.join(&b' ')),
        });
    }
}

/// Helper accessors used only by the query commands.
impl Server {
    pub(crate) fn clients_iter(&self) -> impl Iterator<Item = (ClientId, &Client)> {
        self.clients_map().iter().map(|(id, c)| (*id, c))
    }

    pub(crate) fn channels_iter(&self) -> impl Iterator<Item = (&Vec<u8>, &crate::Channel)> {
        self.channels_map().iter()
    }
}
