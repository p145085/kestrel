//! Channel state and membership.

use std::collections::HashMap;

use crate::client::ClientId;

/// A channel topic and who set it.
#[derive(Debug, Clone)]
pub struct Topic {
    /// The topic text.
    pub text: Vec<u8>,
    /// Mask of whoever set it.
    pub setter: Vec<u8>,
    /// When it was set, in Unix seconds.
    pub set_at: u64,
}

/// A member's status within one channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemberStatus {
    /// Channel operator (`+o`, shown as `@`).
    pub operator: bool,
    /// Voiced (`+v`, shown as `+`).
    pub voice: bool,
}

impl MemberStatus {
    /// The prefix character shown before this member's nick in `NAMES`.
    ///
    /// Only the highest-ranking prefix is returned; clients that negotiate
    /// `multi-prefix` want all of them, which is why [`MemberStatus::prefixes`]
    /// exists separately.
    #[must_use]
    pub fn prefix(self) -> Option<u8> {
        if self.operator {
            Some(b'@')
        } else if self.voice {
            Some(b'+')
        } else {
            None
        }
    }

    /// Every prefix this member holds, highest-ranking first.
    #[must_use]
    pub fn prefixes(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2);
        if self.operator {
            out.push(b'@');
        }
        if self.voice {
            out.push(b'+');
        }
        out
    }

    /// Whether this member may speak in a moderated channel.
    #[must_use]
    pub fn may_speak_when_moderated(self) -> bool {
        self.operator || self.voice
    }
}

/// Channel modes that affect routing and permission checks.
///
/// These are independent protocol flags that happen to be booleans; the IRC
/// mode letters they mirror can be combined freely, so folding them into an
/// enum would misrepresent the protocol rather than simplify it.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelModes {
    /// `+n`: reject messages from clients that are not members.
    pub no_external_messages: bool,
    /// `+t`: only operators may change the topic.
    pub topic_protected: bool,
    /// `+m`: only voiced members and operators may speak.
    pub moderated: bool,
    /// `+i`: joining requires an invitation.
    pub invite_only: bool,
    /// `+s`: hide the channel from listings.
    pub secret: bool,
    /// `+k`: a key is required to join.
    pub key: Option<Vec<u8>>,
    /// `+l`: maximum number of members.
    pub limit: Option<usize>,
}

impl Default for ChannelModes {
    fn default() -> Self {
        // `+nt` is the near-universal default: without `+n` anyone can shout
        // into a channel they are not in, and without `+t` any member can
        // rewrite the topic.
        Self {
            no_external_messages: true,
            topic_protected: true,
            moderated: false,
            invite_only: false,
            secret: false,
            key: None,
            limit: None,
        }
    }
}

impl ChannelModes {
    /// Render as a mode string plus its parameters, as `RPL_CHANNELMODEIS`
    /// wants them.
    #[must_use]
    pub fn render(&self) -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut flags = vec![b'+'];
        let mut params = Vec::new();
        if self.no_external_messages {
            flags.push(b'n');
        }
        if self.topic_protected {
            flags.push(b't');
        }
        if self.moderated {
            flags.push(b'm');
        }
        if self.invite_only {
            flags.push(b'i');
        }
        if self.secret {
            flags.push(b's');
        }
        if let Some(limit) = self.limit {
            flags.push(b'l');
            params.push(limit.to_string().into_bytes());
        }
        if let Some(key) = &self.key {
            flags.push(b'k');
            params.push(key.clone());
        }
        (flags, params)
    }
}

/// One channel.
#[derive(Debug, Clone)]
pub struct Channel {
    /// The name as first created, preserving its original case for display.
    pub(crate) name: Vec<u8>,
    pub(crate) topic: Option<Topic>,
    pub(crate) members: HashMap<ClientId, MemberStatus>,
    pub(crate) modes: ChannelModes,
    pub(crate) created_at: u64,
    /// Clients holding an outstanding invitation to an invite-only channel.
    pub(crate) invited: Vec<ClientId>,
}

impl Channel {
    pub(crate) fn new(name: Vec<u8>, created_at: u64) -> Self {
        Self {
            name,
            topic: None,
            members: HashMap::new(),
            modes: ChannelModes::default(),
            created_at,
            invited: Vec::new(),
        }
    }

    /// The channel name in its original case.
    #[must_use]
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// The topic, if one is set.
    #[must_use]
    pub fn topic(&self) -> Option<&Topic> {
        self.topic.as_ref()
    }

    /// The channel's modes.
    #[must_use]
    pub fn modes(&self) -> &ChannelModes {
        &self.modes
    }

    /// When the channel was created, in Unix seconds.
    #[must_use]
    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    /// Number of members.
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether the channel has no members left.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Whether `client` is a member.
    #[must_use]
    pub fn contains(&self, client: ClientId) -> bool {
        self.members.contains_key(&client)
    }

    /// Whether `client` holds an outstanding invitation.
    #[must_use]
    pub fn is_invited(&self, client: ClientId) -> bool {
        self.invited.contains(&client)
    }

    /// A member's status, if they are in the channel.
    #[must_use]
    pub fn status(&self, client: ClientId) -> Option<MemberStatus> {
        self.members.get(&client).copied()
    }

    /// Every member's id.
    pub fn member_ids(&self) -> impl Iterator<Item = ClientId> + '_ {
        self.members.keys().copied()
    }

    /// Add a member, making them an operator if the channel was empty.
    ///
    /// Whoever creates a channel gets `+o`; otherwise a fresh channel would
    /// have nobody able to set modes or topic.
    pub(crate) fn add_member(&mut self, client: ClientId) -> MemberStatus {
        let status = MemberStatus {
            operator: self.members.is_empty(),
            voice: false,
        };
        self.members.insert(client, status);
        self.invited.retain(|&id| id != client);
        status
    }

    pub(crate) fn remove_member(&mut self, client: ClientId) {
        self.members.remove(&client);
    }
}

#[cfg(test)]
mod tests {
    use super::{Channel, ChannelModes, MemberStatus};
    use crate::client::ClientId;

    #[test]
    fn channel_creator_becomes_operator() {
        let mut chan = Channel::new(b"#chan".to_vec(), 0);
        let first = chan.add_member(ClientId(1));
        let second = chan.add_member(ClientId(2));
        assert!(first.operator, "channel creator should get +o");
        assert!(!second.operator, "later joiners should not");
    }

    #[test]
    fn prefixes_are_ranked_highest_first() {
        let both = MemberStatus {
            operator: true,
            voice: true,
        };
        assert_eq!(both.prefix(), Some(b'@'));
        assert_eq!(both.prefixes(), b"@+");

        let voiced = MemberStatus {
            operator: false,
            voice: true,
        };
        assert_eq!(voiced.prefix(), Some(b'+'));
        assert_eq!(voiced.prefixes(), b"+");

        assert_eq!(MemberStatus::default().prefix(), None);
        assert!(MemberStatus::default().prefixes().is_empty());
    }

    #[test]
    fn new_channels_default_to_plus_nt() {
        let modes = ChannelModes::default();
        assert!(modes.no_external_messages);
        assert!(modes.topic_protected);
        let (flags, params) = modes.render();
        assert_eq!(flags, b"+nt");
        assert!(params.is_empty());
    }

    #[test]
    fn mode_rendering_puts_parameters_in_flag_order() {
        let modes = ChannelModes {
            limit: Some(20),
            key: Some(b"secret".to_vec()),
            ..ChannelModes::default()
        };
        let (flags, params) = modes.render();
        assert_eq!(flags, b"+ntlk");
        assert_eq!(params, vec![b"20".to_vec(), b"secret".to_vec()]);
    }

    #[test]
    fn removing_the_last_member_empties_the_channel() {
        let mut chan = Channel::new(b"#chan".to_vec(), 0);
        chan.add_member(ClientId(1));
        assert!(!chan.is_empty());
        chan.remove_member(ClientId(1));
        assert!(chan.is_empty());
    }
}
