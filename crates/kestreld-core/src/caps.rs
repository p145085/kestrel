//! IRCv3 capabilities: what the server offers, and what each one changes.
//!
//! Capabilities make one event look different to different recipients. A join
//! carries extra parameters for clients that asked for `extended-join`, and
//! nothing extra for those that did not, even though it is the same join. The
//! two mechanisms for that live here: a decoration pass that adds tags to an
//! already-built message, and per-recipient variants for the handful of cases
//! where the message body itself changes.

use kestrel_proto::MessageBuf;

use crate::client::{Client, ClientId};
use crate::datetime::format_iso8601;
use crate::server::{Action, Server};

/// Every capability the server offers, as `name` or `name=value`.
///
/// Advertising one we do not honour is worse than advertising none: a client
/// that enables it changes its own behaviour and then waits for messages that
/// never arrive.
#[must_use]
pub fn supported() -> Vec<String> {
    vec![
        format!("sasl={}", kestreld_services::Mechanism::advertised()),
        "account-notify".to_owned(),
        "account-tag".to_owned(),
        "away-notify".to_owned(),
        "echo-message".to_owned(),
        "extended-join".to_owned(),
        "invite-notify".to_owned(),
        "message-tags".to_owned(),
        "multi-prefix".to_owned(),
        "server-time".to_owned(),
        "setname".to_owned(),
        "standard-replies".to_owned(),
        "draft/account-registration=before-connect".to_owned(),
    ]
}

/// The bare name of a capability token, dropping any `=value`.
#[must_use]
pub fn name_of(token: &str) -> &str {
    token.split('=').next().unwrap_or(token)
}

/// Whether `name` is a capability this server offers.
#[must_use]
pub fn is_supported(name: &str) -> bool {
    supported().iter().any(|token| name_of(token) == name)
}

impl Server {
    /// Add per-recipient tags to messages the handlers have already built.
    ///
    /// Called once over everything a command produced, rather than at each
    /// place a message is constructed: a tag that has to be applied in twenty
    /// call sites is a tag that will be missing from one of them.
    pub(crate) fn decorate(&self, actions: &mut [Action], now: u64) {
        let timestamp = format_iso8601(now);

        for action in actions {
            let Action::Send { to, message } = action else {
                continue;
            };
            let Some(recipient) = self.client(*to) else {
                continue;
            };

            // `server-time` says when the server handled the message, which is
            // what lets a bouncer replay history in the right order.
            if recipient.has_cap("server-time") {
                *message = std::mem::take(message).tag_if_absent("time", &timestamp);
            }

            // Client-only tags are meaningless to a client that did not ask
            // for `message-tags`, so they never reach one.
            if !recipient.has_cap("message-tags") {
                *message = std::mem::take(message).without_client_tags();
            }

            // `account-tag` names the services account behind the nickname.
            // This is the tag call identity will be built on: a nickname is
            // whoever holds it this second, an account is a person.
            if recipient.has_cap("account-tag")
                && let Some(account) = self.account_behind(message.source_bytes())
            {
                *message = std::mem::take(message).tag_if_absent("account", &account);
            }
        }
    }

    /// The services account of whoever sent a message with this source.
    fn account_behind(&self, source: Option<&[u8]>) -> Option<Vec<u8>> {
        let source = source?;
        // A source is `nick!user@host`, or a bare server name.
        let nick = source
            .iter()
            .position(|&b| b == b'!')
            .map_or(source, |i| &source[..i]);
        let id = self.find_nick(nick)?;
        self.client(id)?.account().map(<[u8]>::to_vec)
    }

    /// Send `message` to a channel, choosing a per-recipient variant.
    ///
    /// `variant` is given each member's capabilities and returns the message
    /// that member should see, or `None` to send them nothing.
    pub(crate) fn broadcast_channel_with<F>(
        &self,
        out: &mut Vec<Action>,
        folded_channel: &[u8],
        except: Option<ClientId>,
        variant: F,
    ) where
        F: Fn(&Client) -> Option<MessageBuf>,
    {
        let Some(channel) = self.channel_by_folded(folded_channel) else {
            return;
        };
        for member in channel.member_ids() {
            if Some(member) == except {
                continue;
            }
            let Some(client) = self.client(member) else {
                continue;
            };
            if let Some(message) = variant(client) {
                out.push(Action::Send {
                    to: member,
                    message,
                });
            }
        }
    }

    /// Send `message` to every client holding `capability` that shares a
    /// channel with `about`, plus `about` themselves when `include_self`.
    pub(crate) fn notify_peers_with_cap(
        &self,
        out: &mut Vec<Action>,
        about: ClientId,
        capability: &str,
        message: &MessageBuf,
        include_self: bool,
    ) {
        for peer in self.peers_of(about) {
            if self.client(peer).is_some_and(|c| c.has_cap(capability)) {
                out.push(Action::Send {
                    to: peer,
                    message: message.clone(),
                });
            }
        }
        if include_self {
            out.push(Action::Send {
                to: about,
                message: message.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_supported, name_of, supported};

    #[test]
    fn capability_names_drop_their_values() {
        assert_eq!(name_of("sasl=PLAIN,EXTERNAL"), "sasl");
        assert_eq!(name_of("multi-prefix"), "multi-prefix");
    }

    #[test]
    fn everything_advertised_is_recognised_when_requested() {
        for token in supported() {
            assert!(
                is_supported(name_of(&token)),
                "{token} is advertised but would be refused"
            );
        }
    }

    #[test]
    fn unknown_capabilities_are_not_supported() {
        assert!(!is_supported("not-a-real-cap"));
        assert!(!is_supported(""));
    }

    #[test]
    fn the_advertised_list_has_no_duplicates() {
        let tokens = supported();
        let mut names: Vec<&str> = tokens.iter().map(|t| name_of(t)).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "a capability is advertised twice");
    }
}
