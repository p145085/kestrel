//! State mutations used by the command handlers.
//!
//! These are gathered in one place because every one of them has to keep two
//! structures agreed with each other — a client's channel set and the
//! channel's member map, or a nickname and the lookup table. Scattering them
//! through the handlers is how a server ends up with a ghost sitting in a
//! channel nobody can part from.

use crate::channel::{Channel, Topic};
use crate::client::ClientId;
use crate::server::Server;

impl Server {
    /// Note whether a client is mid-capability-negotiation.
    pub(crate) fn set_cap_negotiating(&mut self, id: ClientId, value: bool) {
        if let Some(client) = self.clients_mut().get_mut(&id) {
            client.cap_negotiating = value;
        }
    }

    /// Record the outcome of a `PASS` check.
    pub(crate) fn set_password_ok(&mut self, id: ClientId, ok: bool) {
        if let Some(client) = self.clients_mut().get_mut(&id) {
            client.password_ok = ok;
        }
    }

    /// Record the username and realname from `USER`.
    pub(crate) fn set_user(&mut self, id: ClientId, user: &[u8], realname: &[u8]) {
        // No ident lookup is performed, so the username is prefixed with `~`
        // to say so — the same convention every other ircd uses. Without it a
        // client could claim any username and have it look verified.
        let mut username = vec![b'~'];
        username.extend(
            user.iter()
                .copied()
                .filter(|b| b.is_ascii_graphic() && !matches!(b, b'@' | b'!' | b'%'))
                .take(10),
        );
        let mut realname = realname.to_vec();
        realname.truncate(400);

        if let Some(client) = self.clients_mut().get_mut(&id) {
            client.user = Some(username);
            client.realname = realname;
        }
    }

    /// Set or clear a client's away message.
    pub(crate) fn set_away(&mut self, id: ClientId, away: Option<Vec<u8>>) {
        if let Some(client) = self.clients_mut().get_mut(&id) {
            client.away = away;
        }
    }

    /// Clamp a quit or part reason to the configured length.
    pub(crate) fn truncate_reason(&self, reason: &[u8]) -> Vec<u8> {
        let mut reason = reason.to_vec();
        reason.truncate(self.config().max_reason_len);
        reason
    }

    /// Add a client to a channel, creating it if it does not exist.
    pub(crate) fn insert_member(
        &mut self,
        folded: &[u8],
        display: Vec<u8>,
        id: ClientId,
        now: u64,
    ) {
        self.channels_mut()
            .entry(folded.to_vec())
            .or_insert_with(|| Channel::new(display, now))
            .add_member(id);
        if let Some(client) = self.clients_mut().get_mut(&id) {
            client.channels.insert(folded.to_vec());
        }
    }

    /// Remove a client from a channel, dropping the channel if it empties.
    pub(crate) fn remove_member(&mut self, folded: &[u8], id: ClientId) {
        if let Some(channel) = self.channels_mut().get_mut(folded) {
            channel.remove_member(id);
            if channel.is_empty() {
                self.channels_mut().remove(folded);
            }
        }
        if let Some(client) = self.clients_mut().get_mut(&id) {
            client.channels.remove(folded);
        }
    }

    /// Set or clear a channel's topic.
    pub(crate) fn set_topic(&mut self, folded: &[u8], topic: Option<Topic>) {
        if let Some(channel) = self.channels_mut().get_mut(folded) {
            channel.topic = topic;
        }
    }
}
