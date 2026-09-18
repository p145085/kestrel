//! Channel and member state the session keeps up to date.

use std::collections::HashMap;

use crate::isupport::ISupport;

/// One member of a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Their nickname, in the case the server used.
    pub nick: Vec<u8>,
    /// Membership prefixes they hold, highest-ranking first, such as `@+`.
    pub prefixes: Vec<u8>,
    /// Their services account, when the server has told us.
    pub account: Option<Vec<u8>>,
    /// Whether they are marked away.
    pub away: bool,
}

impl Member {
    /// A member with no prefixes.
    #[must_use]
    pub fn new(nick: Vec<u8>) -> Self {
        Self {
            nick,
            prefixes: Vec::new(),
            account: None,
            away: false,
        }
    }

    /// The highest-ranking prefix they hold, if any.
    #[must_use]
    pub fn top_prefix(&self) -> Option<u8> {
        self.prefixes.first().copied()
    }

    /// Their nickname with its highest prefix, as most clients display it.
    #[must_use]
    pub fn display(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.nick.len() + 1);
        if let Some(prefix) = self.top_prefix() {
            out.push(prefix);
        }
        out.extend_from_slice(&self.nick);
        out
    }
}

/// A channel we are in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Channel {
    /// The name in the case the server used.
    pub name: Vec<u8>,
    /// The topic, when there is one.
    pub topic: Option<Vec<u8>>,
    /// Who set the topic, when the server said.
    pub topic_setter: Option<Vec<u8>>,
    /// Folded nickname to member.
    members: HashMap<Vec<u8>, Member>,
    /// Members accumulated from `RPL_NAMREPLY` but not yet committed.
    ///
    /// A names listing arrives across several messages and ends with
    /// `RPL_ENDOFNAMES`. Replacing the roster only at the end means a slow
    /// listing never shows a half-empty channel.
    pending: Vec<Member>,
    /// Whether a names listing is being collected.
    collecting: bool,
}

impl Channel {
    /// An empty channel with the given name.
    #[must_use]
    pub fn new(name: Vec<u8>) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }

    /// How many members it has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether the roster is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Look up a member.
    #[must_use]
    pub fn member(&self, support: &ISupport, nick: &[u8]) -> Option<&Member> {
        self.members.get(&support.fold(nick))
    }

    /// Whether `nick` is in the channel.
    #[must_use]
    pub fn contains(&self, support: &ISupport, nick: &[u8]) -> bool {
        self.members.contains_key(&support.fold(nick))
    }

    /// Every member, in no particular order.
    pub fn members(&self) -> impl Iterator<Item = &Member> {
        self.members.values()
    }

    /// Members sorted for display: by rank, then by folded nickname.
    #[must_use]
    pub fn sorted_members(&self, support: &ISupport) -> Vec<&Member> {
        let ranks = support.prefix_symbols();
        let mut members: Vec<&Member> = self.members.values().collect();
        members.sort_by_cached_key(|member| {
            let rank = member
                .top_prefix()
                .and_then(|prefix| ranks.iter().position(|&r| r == prefix))
                // Members with no prefix sort after everyone who has one.
                .unwrap_or(ranks.len());
            (rank, support.fold(&member.nick))
        });
        members
    }

    pub(crate) fn add(&mut self, support: &ISupport, member: Member) {
        self.members.insert(support.fold(&member.nick), member);
    }

    pub(crate) fn remove(&mut self, support: &ISupport, nick: &[u8]) -> Option<Member> {
        self.members.remove(&support.fold(nick))
    }

    pub(crate) fn rename(&mut self, support: &ISupport, old: &[u8], new: &[u8]) -> bool {
        let Some(mut member) = self.members.remove(&support.fold(old)) else {
            return false;
        };
        member.nick = new.to_vec();
        self.members.insert(support.fold(new), member);
        true
    }

    pub(crate) fn set_away(&mut self, support: &ISupport, nick: &[u8], away: bool) -> bool {
        let Some(member) = self.members.get_mut(&support.fold(nick)) else {
            return false;
        };
        member.away = away;
        true
    }

    pub(crate) fn set_account(
        &mut self,
        support: &ISupport,
        nick: &[u8],
        account: Option<Vec<u8>>,
    ) {
        if let Some(member) = self.members.get_mut(&support.fold(nick)) {
            member.account = account;
        }
    }

    /// Apply a membership prefix change from a `MODE`.
    pub(crate) fn set_prefix(
        &mut self,
        support: &ISupport,
        nick: &[u8],
        symbol: u8,
        adding: bool,
    ) -> bool {
        let ranks = support.prefix_symbols();
        let Some(member) = self.members.get_mut(&support.fold(nick)) else {
            return false;
        };
        if adding {
            if member.prefixes.contains(&symbol) {
                return false;
            }
            member.prefixes.push(symbol);
            // Keep them ranked, so `top_prefix` means what it says.
            member
                .prefixes
                .sort_by_key(|p| ranks.iter().position(|r| r == p).unwrap_or(usize::MAX));
        } else {
            let before = member.prefixes.len();
            member.prefixes.retain(|&p| p != symbol);
            if member.prefixes.len() == before {
                return false;
            }
        }
        true
    }

    pub(crate) fn begin_names(&mut self) {
        self.pending.clear();
        self.collecting = true;
    }

    pub(crate) fn push_name(&mut self, member: Member) {
        if !self.collecting {
            self.begin_names();
        }
        self.pending.push(member);
    }

    /// Replace the roster with what the listing collected.
    pub(crate) fn commit_names(&mut self, support: &ISupport) {
        if !self.collecting {
            return;
        }
        self.members = self
            .pending
            .drain(..)
            .map(|member| (support.fold(&member.nick), member))
            .collect();
        self.collecting = false;
    }
}

#[cfg(test)]
mod tests {
    use super::{Channel, Member};
    use crate::isupport::ISupport;

    fn member(nick: &str, prefixes: &str) -> Member {
        Member {
            nick: nick.as_bytes().to_vec(),
            prefixes: prefixes.as_bytes().to_vec(),
            account: None,
            away: false,
        }
    }

    #[test]
    fn members_are_looked_up_with_the_networks_casemapping() {
        let support = ISupport::new();
        let mut channel = Channel::new(b"#chan".to_vec());
        channel.add(&support, member("Alice", ""));

        assert!(channel.contains(&support, b"alice"));
        assert!(channel.contains(&support, b"ALICE"));
        assert!(!channel.contains(&support, b"bob"));
    }

    #[test]
    fn display_shows_only_the_highest_prefix() {
        assert_eq!(member("alice", "@+").display(), b"@alice");
        assert_eq!(member("bob", "").display(), b"bob");
    }

    #[test]
    fn members_sort_by_rank_then_name() {
        let support = ISupport::new();
        let mut channel = Channel::new(b"#chan".to_vec());
        for m in [
            member("zoe", "@"),
            member("alice", ""),
            member("bob", "+"),
            member("adam", "@"),
        ] {
            channel.add(&support, m);
        }

        let sorted: Vec<&[u8]> = channel
            .sorted_members(&support)
            .iter()
            .map(|m| m.nick.as_slice())
            .collect();
        assert_eq!(sorted, [&b"adam"[..], b"zoe", b"bob", b"alice"]);
    }

    #[test]
    fn renaming_moves_a_member_to_their_new_key() {
        let support = ISupport::new();
        let mut channel = Channel::new(b"#chan".to_vec());
        channel.add(&support, member("alice", "@"));

        assert!(channel.rename(&support, b"alice", b"alice2"));
        assert!(!channel.contains(&support, b"alice"));
        let renamed = channel.member(&support, b"alice2").unwrap();
        assert_eq!(renamed.prefixes, b"@", "status should survive a rename");
    }

    #[test]
    fn renaming_someone_absent_reports_it() {
        let support = ISupport::new();
        let mut channel = Channel::new(b"#chan".to_vec());
        assert!(!channel.rename(&support, b"nobody", b"somebody"));
    }

    #[test]
    fn prefixes_stay_ranked_however_they_are_granted() {
        let support = ISupport::new();
        let mut channel = Channel::new(b"#chan".to_vec());
        channel.add(&support, member("alice", ""));

        // Voice first, then op: the op prefix must still sort to the front.
        assert!(channel.set_prefix(&support, b"alice", b'+', true));
        assert!(channel.set_prefix(&support, b"alice", b'@', true));
        assert_eq!(channel.member(&support, b"alice").unwrap().prefixes, b"@+");
        assert_eq!(
            channel.member(&support, b"alice").unwrap().display(),
            b"@alice"
        );

        assert!(channel.set_prefix(&support, b"alice", b'@', false));
        assert_eq!(channel.member(&support, b"alice").unwrap().prefixes, b"+");
    }

    #[test]
    fn granting_a_prefix_twice_changes_nothing() {
        let support = ISupport::new();
        let mut channel = Channel::new(b"#chan".to_vec());
        channel.add(&support, member("alice", "@"));
        assert!(!channel.set_prefix(&support, b"alice", b'@', true));
    }

    #[test]
    fn a_names_listing_replaces_the_roster_only_when_it_ends() {
        // Committing each batch as it arrives would show a half-empty channel
        // while a large listing is still streaming in.
        let support = ISupport::new();
        let mut channel = Channel::new(b"#chan".to_vec());
        channel.add(&support, member("stale", ""));

        channel.begin_names();
        channel.push_name(member("alice", "@"));
        assert!(
            channel.contains(&support, b"stale"),
            "the old roster should stand until the listing ends"
        );

        channel.push_name(member("bob", ""));
        channel.commit_names(&support);

        assert!(!channel.contains(&support, b"stale"));
        assert_eq!(channel.len(), 2);
    }

    #[test]
    fn committing_without_a_listing_is_a_no_op() {
        let support = ISupport::new();
        let mut channel = Channel::new(b"#chan".to_vec());
        channel.add(&support, member("alice", ""));
        channel.commit_names(&support);
        assert_eq!(channel.len(), 1);
    }
}
