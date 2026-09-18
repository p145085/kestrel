//! The server state machine.
//!
//! [`Server`] is sans-io and sans-clock: it never touches a socket and never
//! reads the time. Input arrives as parsed messages plus an explicit
//! timestamp; output is a list of [`Action`]s for the caller to perform. That
//! makes the whole of the server's behaviour testable in-process, with no
//! network, no scheduler, and no flakiness.

use std::collections::{HashMap, HashSet};

use kestrel_proto::{Message, MessageBuf, numeric};
use kestreld_services::AccountStore;

use crate::channel::Channel;
use crate::client::{Client, ClientId, RegistrationState};
use crate::config::ServerConfig;

/// Something the caller should do as a result of handling input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Write a message to a client.
    Send {
        /// Which client.
        to: ClientId,
        /// What to write.
        message: MessageBuf,
    },
    /// Close a client's connection, after flushing anything already queued.
    Close {
        /// Which client.
        client: ClientId,
    },
}

impl Action {
    fn send(to: ClientId, message: MessageBuf) -> Self {
        Self::Send { to, message }
    }
}

/// An IRC server.
#[derive(Debug)]
pub struct Server {
    config: ServerConfig,
    clients: HashMap<ClientId, Client>,
    /// Folded nickname to client, so lookups honour the network's casemapping.
    nicks: HashMap<Vec<u8>, ClientId>,
    /// Folded channel name to channel.
    channels: HashMap<Vec<u8>, Channel>,
    next_id: u64,
    created_at: u64,
    max_local_users: usize,
    accounts: AccountStore,
}

impl Server {
    /// Create a server. `now` is the current time in Unix seconds.
    #[must_use]
    pub fn new(config: ServerConfig, now: u64) -> Self {
        Self {
            config,
            clients: HashMap::new(),
            nicks: HashMap::new(),
            channels: HashMap::new(),
            next_id: 1,
            created_at: now,
            max_local_users: 0,
            accounts: AccountStore::new(),
        }
    }

    /// The account store.
    #[must_use]
    pub fn accounts(&self) -> &AccountStore {
        &self.accounts
    }

    /// The account store, mutably, for registration and loading from disk.
    pub fn accounts_mut(&mut self) -> &mut AccountStore {
        &mut self.accounts
    }

    /// Record the fingerprint of a client's TLS certificate.
    ///
    /// The transport supplies this; a client can never set its own, which is
    /// the whole basis of authenticating by certificate.
    pub fn set_certificate_fingerprint(&mut self, id: ClientId, fingerprint: String) {
        if let Some(client) = self.clients.get_mut(&id) {
            client.certificate_fingerprint = Some(fingerprint);
        }
    }

    /// The server's configuration.
    #[must_use]
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// When the server started, in Unix seconds.
    #[must_use]
    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    /// Register a newly accepted connection and return its id.
    ///
    /// `host` is the hostname to show in this client's mask.
    pub fn connect(&mut self, host: Vec<u8>) -> ClientId {
        let id = ClientId(self.next_id);
        self.next_id += 1;
        self.clients.insert(id, Client::new(id, host));
        self.max_local_users = self.max_local_users.max(self.clients.len());
        id
    }

    /// Look up a client.
    #[must_use]
    pub fn client(&self, id: ClientId) -> Option<&Client> {
        self.clients.get(&id)
    }

    /// Look up a channel by name, honouring the network's casemapping.
    #[must_use]
    pub fn channel(&self, name: &[u8]) -> Option<&Channel> {
        self.channels.get(&self.fold(name))
    }

    /// Number of connected clients.
    #[must_use]
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Number of clients that have completed registration.
    #[must_use]
    pub fn registered_count(&self) -> usize {
        self.clients.values().filter(|c| c.is_registered()).count()
    }

    /// Number of channels that currently exist.
    #[must_use]
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// Highest number of simultaneous connections seen so far.
    #[must_use]
    pub fn max_local_users(&self) -> usize {
        self.max_local_users
    }

    // --- internal helpers --------------------------------------------------

    pub(crate) fn clients_map(&self) -> &HashMap<ClientId, Client> {
        &self.clients
    }

    pub(crate) fn channels_map(&self) -> &HashMap<Vec<u8>, Channel> {
        &self.channels
    }

    pub(crate) fn clients_mut(&mut self) -> &mut HashMap<ClientId, Client> {
        &mut self.clients
    }

    pub(crate) fn channels_mut(&mut self) -> &mut HashMap<Vec<u8>, Channel> {
        &mut self.channels
    }

    /// Look up a channel by an already-folded name.
    #[must_use]
    pub fn channel_by_folded(&self, folded: &[u8]) -> Option<&Channel> {
        self.channels.get(folded)
    }

    pub(crate) fn fold(&self, name: &[u8]) -> Vec<u8> {
        self.config.casemapping.fold(name)
    }

    pub(crate) fn find_nick(&self, nick: &[u8]) -> Option<ClientId> {
        self.nicks.get(&self.fold(nick)).copied()
    }

    pub(crate) fn server_name(&self) -> Vec<u8> {
        self.config.server_name.clone()
    }

    /// Build a numeric reply addressed to `id`.
    ///
    /// Numerics always carry the recipient's nick as their first parameter,
    /// or `*` if they have not chosen one yet.
    pub(crate) fn numeric(&self, id: ClientId, code: u16) -> MessageBuf {
        let target = self
            .clients
            .get(&id)
            .map_or(&b"*"[..], Client::nick_or_star)
            .to_vec();
        MessageBuf::numeric(code)
            .source(self.server_name())
            .param(target)
    }

    /// Every other client that shares at least one channel with `id`.
    ///
    /// This is the audience for nick changes and quits: the people who can
    /// see the client and therefore need to be told.
    pub(crate) fn peers_of(&self, id: ClientId) -> HashSet<ClientId> {
        let mut peers = HashSet::new();
        let Some(client) = self.clients.get(&id) else {
            return peers;
        };
        for folded in &client.channels {
            if let Some(channel) = self.channels.get(folded) {
                peers.extend(channel.member_ids());
            }
        }
        peers.remove(&id);
        peers
    }

    /// Send `message` to every member of `channel`, optionally skipping one.
    pub(crate) fn broadcast_channel(
        &self,
        out: &mut Vec<Action>,
        folded_channel: &[u8],
        message: &MessageBuf,
        except: Option<ClientId>,
    ) {
        let Some(channel) = self.channels.get(folded_channel) else {
            return;
        };
        for member in channel.member_ids() {
            if Some(member) != except {
                out.push(Action::send(member, message.clone()));
            }
        }
    }

    /// Remove a client from all state, telling everyone who could see them.
    ///
    /// Used for both `QUIT` and an abrupt socket close, so the two cannot
    /// diverge and leave a ghost in a channel.
    fn remove_client(&mut self, id: ClientId, reason: &[u8], out: &mut Vec<Action>) {
        let Some(client) = self.clients.get(&id) else {
            return;
        };

        if client.is_registered() {
            let quit = MessageBuf::new("QUIT")
                .source(client.mask())
                .trailing(reason.to_vec());
            for peer in self.peers_of(id) {
                out.push(Action::send(peer, quit.clone()));
            }
        }

        let folded_channels: Vec<Vec<u8>> = client.channels.iter().cloned().collect();
        let folded_nick = client.nick.as_deref().map(|n| self.fold(n));

        for folded in folded_channels {
            if let Some(channel) = self.channels.get_mut(&folded) {
                channel.remove_member(id);
                if channel.is_empty() {
                    self.channels.remove(&folded);
                }
            }
        }

        if let Some(folded_nick) = folded_nick {
            // Only clear the mapping if it still points at us: a collision
            // recovery could already have handed the nick to someone else.
            if self.nicks.get(&folded_nick) == Some(&id) {
                self.nicks.remove(&folded_nick);
            }
        }

        self.clients.remove(&id);
    }

    /// Handle a client sending `QUIT`, or the server ending the connection.
    pub fn quit(&mut self, id: ClientId, reason: &[u8], out: &mut Vec<Action>) {
        self.remove_client(id, reason, out);
        out.push(Action::Close { client: id });
    }

    /// Handle a connection that dropped without a `QUIT`.
    pub fn disconnect(&mut self, id: ClientId, reason: &[u8], out: &mut Vec<Action>) {
        self.remove_client(id, reason, out);
    }

    /// Claim `nick` for `id`, updating the lookup table.
    pub(crate) fn set_nick(&mut self, id: ClientId, nick: &[u8]) {
        let folded = self.fold(nick);
        if let Some(client) = self.clients.get_mut(&id)
            && let Some(old) = client.nick.replace(nick.to_vec())
        {
            let old_folded = self.config.casemapping.fold(&old);
            // Only drop the old mapping if the fold actually changed;
            // re-casing your own nick must not free it for someone else.
            if old_folded != folded {
                self.nicks.remove(&old_folded);
            }
        }
        self.nicks.insert(folded, id);
    }

    // --- dispatch ----------------------------------------------------------

    /// Handle one message from a client.
    ///
    /// `now` is the current time in Unix seconds, used for topic and channel
    /// creation timestamps.
    pub fn handle(&mut self, id: ClientId, msg: &Message<'_>, now: u64, out: &mut Vec<Action>) {
        if !self.clients.contains_key(&id) {
            return;
        }

        let command = msg.command().to_ascii_uppercase();

        // Commands permitted before registration completes. Everything else
        // gets ERR_NOTREGISTERED, so an unregistered connection cannot reach
        // channel or messaging state.
        let pre_registration = matches!(
            command.as_slice(),
            b"CAP"
                | b"PASS"
                | b"AUTHENTICATE"
                | b"NICK"
                | b"USER"
                | b"QUIT"
                | b"PING"
                | b"PONG"
                | b"ERROR"
        );

        let registered = self.clients[&id].is_registered();
        if !registered && !pre_registration {
            out.push(Action::send(
                id,
                self.numeric(id, numeric::ERR_NOTREGISTERED)
                    .trailing("You have not registered"),
            ));
            return;
        }

        match command.as_slice() {
            b"CAP" => self.cmd_cap(id, msg, now, out),
            b"PASS" => self.cmd_pass(id, msg, out),
            b"AUTHENTICATE" => self.cmd_authenticate(id, msg, out),
            b"NICK" => self.cmd_nick(id, msg, now, out),
            b"USER" => self.cmd_user(id, msg, now, out),
            b"PING" => self.cmd_ping(id, msg, out),
            b"PONG" => {}
            b"QUIT" => self.cmd_quit(id, msg, out),
            b"JOIN" => self.cmd_join(id, msg, now, out),
            b"PART" => self.cmd_part(id, msg, out),
            b"PRIVMSG" => self.cmd_privmsg(id, msg, false, out),
            b"NOTICE" => self.cmd_privmsg(id, msg, true, out),
            b"TOPIC" => self.cmd_topic(id, msg, now, out),
            b"MODE" => self.cmd_mode(id, msg, now, out),
            b"KICK" => self.cmd_kick(id, msg, out),
            b"INVITE" => self.cmd_invite(id, msg, out),
            b"NAMES" => self.cmd_names(id, msg, out),
            b"MOTD" => self.send_motd(id, out),
            b"LUSERS" => self.send_lusers(id, out),
            b"AWAY" => self.cmd_away(id, msg, out),
            b"WHOIS" => self.cmd_whois(id, msg, out),
            b"WHO" => self.cmd_who(id, msg, out),
            b"LIST" => self.cmd_list(id, msg, out),
            b"ISON" => self.cmd_ison(id, msg, out),
            b"USERHOST" => self.cmd_userhost(id, msg, out),
            _ => {
                out.push(Action::send(
                    id,
                    self.numeric(id, numeric::ERR_UNKNOWNCOMMAND)
                        .param(msg.command().to_vec())
                        .trailing("Unknown command"),
                ));
            }
        }
    }

    /// Whether registration can now complete, and if so, do it.
    pub(crate) fn try_complete_registration(
        &mut self,
        id: ClientId,
        _now: u64,
        out: &mut Vec<Action>,
    ) {
        let password_required = self.config.password.is_some();
        let Some(client) = self.clients.get(&id) else {
            return;
        };
        if !client.can_register(password_required) {
            return;
        }
        if let Some(client) = self.clients.get_mut(&id) {
            client.state = RegistrationState::Registered;
        }
        self.send_welcome(id, out);
    }
}

/// Convenience for tests and callers that want one action list per call.
impl Server {
    /// Handle a message and return the actions it produced.
    #[must_use]
    pub fn handle_to_vec(&mut self, id: ClientId, msg: &Message<'_>, now: u64) -> Vec<Action> {
        let mut out = Vec::new();
        self.handle(id, msg, now, &mut out);
        out
    }
}
