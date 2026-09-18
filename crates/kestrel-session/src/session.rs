//! The client session state machine.

use std::collections::{HashMap, HashSet};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use kestrel_proto::{Message, MessageBuf, numeric};

use crate::config::{Sasl, SessionConfig};
use crate::event::{Ended, Event, MessageKind, Sender, Target};
use crate::isupport::ISupport;
use crate::state::{Channel, Member};

/// Longest payload one `AUTHENTICATE` line may carry.
///
/// A response longer than this is split into chunks of exactly this size; a
/// shorter chunk ends the message. The server side of this rule lives in
/// `kestreld-services`.
const MAX_SASL_CHUNK: usize = 400;

/// What the transport should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Write this message to the server.
    Send(MessageBuf),
    /// Close the connection.
    Disconnect,
}

/// The result of feeding the session something.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Messages to write.
    pub actions: Vec<Action>,
    /// Things that happened, for a UI to render.
    pub events: Vec<Event>,
}

impl Outcome {
    fn send(&mut self, message: MessageBuf) {
        self.actions.push(Action::Send(message));
    }

    fn emit(&mut self, event: Event) {
        self.events.push(event);
    }
}

/// How far through connecting the session is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Nothing sent yet.
    Fresh,
    /// Registering: capabilities, authentication, nickname.
    Registering,
    /// Registered and settled.
    Ready,
    /// Finished, for better or worse.
    Ended,
}

/// A client's view of one server connection.
#[derive(Debug)]
pub struct Session {
    config: SessionConfig,
    phase: Phase,
    nick: Vec<u8>,
    /// How many alternatives have been tried after a collision.
    nick_attempt: usize,
    support: ISupport,
    available_caps: HashMap<String, Option<String>>,
    enabled_caps: HashSet<String>,
    /// Set while `CAP LS` is arriving in more than one message.
    caps_pending: bool,
    /// Set once `CAP REQ` has been sent and its reply is awaited.
    caps_requested: bool,
    sasl_done: bool,
    account: Option<Vec<u8>>,
    channels: HashMap<Vec<u8>, Channel>,
}

impl Session {
    /// A session that has not connected yet.
    #[must_use]
    pub fn new(config: SessionConfig) -> Self {
        let nick = config.nick.clone();
        Self {
            config,
            phase: Phase::Fresh,
            nick,
            nick_attempt: 0,
            support: ISupport::new(),
            available_caps: HashMap::new(),
            enabled_caps: HashSet::new(),
            caps_pending: false,
            caps_requested: false,
            sasl_done: false,
            account: None,
            channels: HashMap::new(),
        }
    }

    /// Our current nickname.
    #[must_use]
    pub fn nick(&self) -> &[u8] {
        &self.nick
    }

    /// The account we are logged in as, if any.
    #[must_use]
    pub fn account(&self) -> Option<&[u8]> {
        self.account.as_deref()
    }

    /// What the server said about itself.
    #[must_use]
    pub fn isupport(&self) -> &ISupport {
        &self.support
    }

    /// Whether registration has completed.
    #[must_use]
    pub fn is_registered(&self) -> bool {
        self.phase == Phase::Ready
    }

    /// The capabilities in effect.
    #[must_use]
    pub fn enabled_caps(&self) -> &HashSet<String> {
        &self.enabled_caps
    }

    /// A channel we are in.
    #[must_use]
    pub fn channel(&self, name: &[u8]) -> Option<&Channel> {
        self.channels.get(&self.support.fold(name))
    }

    /// Every channel we are in.
    pub fn channels(&self) -> impl Iterator<Item = &Channel> {
        self.channels.values()
    }

    /// Whether `nick` is us.
    #[must_use]
    pub fn is_me(&self, nick: &[u8]) -> bool {
        self.support.eq(&self.nick, nick)
    }

    // --- driving the session ----------------------------------------------

    /// Begin registration. Call this once the transport is connected.
    pub fn start(&mut self) -> Outcome {
        let mut out = Outcome::default();
        if self.phase != Phase::Fresh {
            return out;
        }
        self.phase = Phase::Registering;

        // CAP LS goes first so the server holds registration open while we
        // negotiate. `302` asks for values alongside names.
        out.send(MessageBuf::new("CAP").param("LS").param("302"));
        self.caps_pending = true;

        if let Some(password) = &self.config.server_password {
            out.send(MessageBuf::new("PASS").param(password.clone()));
        }
        out.send(MessageBuf::new("NICK").param(self.nick.clone()));
        out.send(
            MessageBuf::new("USER")
                .param(self.config.username.clone())
                .param("0")
                .param("*")
                .trailing(self.config.realname.clone()),
        );
        out
    }

    /// Feed one message from the server.
    #[must_use]
    pub fn handle(&mut self, msg: &Message<'_>) -> Outcome {
        let mut out = Outcome::default();
        let command = msg.command().to_ascii_uppercase();

        match command.as_slice() {
            b"PING" => {
                let token = msg.param(0).unwrap_or(b"").to_vec();
                out.send(MessageBuf::new("PONG").trailing(token));
            }
            b"ERROR" => {
                let text = lossy(msg.param(0).unwrap_or(b"connection closed"));
                self.phase = Phase::Ended;
                out.emit(Event::Ended(Ended::ServerError(text)));
            }
            b"CAP" => self.handle_cap(msg, &mut out),
            b"AUTHENTICATE" => self.handle_authenticate(msg, &mut out),
            b"PRIVMSG" => self.handle_message(msg, MessageKind::Privmsg, &mut out),
            b"NOTICE" => self.handle_message(msg, MessageKind::Notice, &mut out),
            b"TAGMSG" => self.handle_tagmsg(msg, &mut out),
            b"JOIN" => self.handle_join(msg, &mut out),
            b"PART" => self.handle_part(msg, &mut out),
            b"QUIT" => self.handle_quit(msg, &mut out),
            b"KICK" => self.handle_kick(msg, &mut out),
            b"NICK" => self.handle_nick(msg, &mut out),
            b"MODE" => self.handle_mode(msg, &mut out),
            b"TOPIC" => self.handle_topic(msg, &mut out),
            b"AWAY" => self.handle_away(msg, &mut out),
            b"ACCOUNT" => self.handle_account(msg, &mut out),
            b"INVITE" => Self::handle_invite(msg, &mut out),
            b"CALL" => Self::handle_call(msg, &mut out),
            b"FAIL" | b"WARN" | b"NOTE" => {
                Self::handle_standard_reply(&command, msg, &mut out);
            }
            _ => {
                if let Some(code) = msg.numeric() {
                    self.handle_numeric(code, msg, &mut out);
                } else {
                    out.emit(Event::Raw(MessageBuf::from(msg)));
                }
            }
        }
        out
    }

    // --- capabilities and authentication ----------------------------------

    fn handle_cap(&mut self, msg: &Message<'_>, out: &mut Outcome) {
        // CAP <target> <subcommand> [*] :<caps>
        let Some(subcommand) = msg.param(1) else {
            return;
        };
        match subcommand.to_ascii_uppercase().as_slice() {
            b"LS" | b"NEW" => {
                // A `*` before the list means more messages follow.
                let more = msg.param(2) == Some(b"*");
                let list = msg.params().last().copied().unwrap_or(b"");
                self.absorb_caps(list);
                if !more {
                    self.caps_pending = false;
                    self.request_caps(out);
                }
            }
            b"ACK" => {
                let list = msg.params().last().copied().unwrap_or(b"");
                for token in split_words(list) {
                    if let Some(name) = token.strip_prefix('-') {
                        self.enabled_caps.remove(name);
                    } else {
                        self.enabled_caps.insert(token.clone());
                    }
                }
                let mut enabled: Vec<String> = self.enabled_caps.iter().cloned().collect();
                enabled.sort();
                out.emit(Event::CapsEnabled(enabled));
                self.after_caps(out);
            }
            b"NAK" => {
                // Nothing was enabled, so there is nothing to undo. Carry on
                // rather than failing: the capabilities are all optional.
                self.after_caps(out);
            }
            b"DEL" => {
                let list = msg.params().last().copied().unwrap_or(b"");
                for token in split_words(list) {
                    self.enabled_caps.remove(&token);
                    self.available_caps.remove(&token);
                }
            }
            _ => {}
        }
    }

    fn absorb_caps(&mut self, list: &[u8]) {
        for token in split_words(list) {
            let (name, value) = match token.split_once('=') {
                Some((name, value)) => (name.to_owned(), Some(value.to_owned())),
                None => (token, None),
            };
            self.available_caps.insert(name, value);
        }
    }

    fn request_caps(&mut self, out: &mut Outcome) {
        let wanted: Vec<String> = self
            .config
            .wanted_caps
            .iter()
            .filter(|name| self.available_caps.contains_key(*name))
            .cloned()
            .collect();

        if wanted.is_empty() {
            self.after_caps(out);
            return;
        }
        self.caps_requested = true;
        out.send(
            MessageBuf::new("CAP")
                .param("REQ")
                .trailing(wanted.join(" ")),
        );
    }

    /// Decide what happens once capability negotiation has settled.
    fn after_caps(&mut self, out: &mut Outcome) {
        if self.phase != Phase::Registering {
            return; // A later CAP NEW/ACK, long after registration.
        }
        if self.should_authenticate() {
            self.begin_sasl(out);
            return;
        }
        self.end_caps(out);
    }

    fn should_authenticate(&self) -> bool {
        !self.sasl_done && self.config.sasl.is_some() && self.enabled_caps.contains("sasl")
    }

    fn begin_sasl(&mut self, out: &mut Outcome) {
        let mechanism = match &self.config.sasl {
            Some(Sasl::Plain { .. }) => "PLAIN",
            Some(Sasl::External) => "EXTERNAL",
            None => return,
        };
        out.send(MessageBuf::new("AUTHENTICATE").param(mechanism));
    }

    fn end_caps(&mut self, out: &mut Outcome) {
        if self.caps_requested || !self.available_caps.is_empty() {
            out.send(MessageBuf::new("CAP").param("END"));
        }
    }

    fn handle_authenticate(&mut self, msg: &Message<'_>, out: &mut Outcome) {
        // The server asks for our response with `AUTHENTICATE +`.
        if msg.param(0) != Some(b"+") {
            return;
        }
        let payload = match &self.config.sasl {
            Some(Sasl::Plain { account, password }) => {
                let mut raw = Vec::with_capacity(account.len() + password.len() + 2);
                raw.push(0);
                raw.extend_from_slice(account);
                raw.push(0);
                raw.extend_from_slice(password);
                BASE64.encode(raw)
            }
            // EXTERNAL proves identity with the TLS certificate already sent,
            // so the payload is empty.
            Some(Sasl::External) => String::new(),
            None => return,
        };

        for chunk in chunk_payload(&payload) {
            out.send(MessageBuf::new("AUTHENTICATE").param(chunk));
        }
    }

    // --- registration numerics --------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn handle_numeric(&mut self, code: u16, msg: &Message<'_>, out: &mut Outcome) {
        match code {
            numeric::RPL_WELCOME => {
                // The server's idea of our nickname wins: it may have
                // truncated or changed what we asked for.
                if let Some(nick) = msg.param(0) {
                    self.nick = nick.to_vec();
                }
                self.phase = Phase::Ready;
                out.emit(Event::Registered {
                    nick: self.nick.clone(),
                });
                for channel in &self.config.autojoin {
                    out.send(MessageBuf::new("JOIN").param(channel.clone()));
                }
            }
            numeric::RPL_ISUPPORT => self.support.absorb(msg),
            numeric::ERR_NICKNAMEINUSE | numeric::ERR_ERRONEUSNICKNAME => {
                if self.phase == Phase::Registering {
                    self.try_next_nick(out);
                } else {
                    Self::emit_numeric(code, msg, out);
                }
            }
            numeric::RPL_LOGGEDIN => {
                if let Some(account) = msg.param(2) {
                    self.account = Some(account.to_vec());
                }
            }
            numeric::RPL_SASLSUCCESS => {
                self.sasl_done = true;
                out.emit(Event::LoggedIn {
                    account: self.account.clone().unwrap_or_default(),
                });
                self.end_caps(out);
            }
            numeric::ERR_SASLFAIL
            | numeric::ERR_SASLTOOLONG
            | numeric::ERR_SASLABORTED
            | numeric::ERR_NICKLOCKED => {
                self.sasl_done = true;
                out.emit(Event::LoginFailed {
                    reason: trailing_text(msg).unwrap_or_else(|| "authentication failed".into()),
                });
                // Registration continues unauthenticated rather than stalling:
                // a failed login should not cost the user the connection.
                self.end_caps(out);
            }
            numeric::ERR_SASLALREADY => self.sasl_done = true,
            numeric::RPL_NAMREPLY => self.handle_names(msg),
            numeric::RPL_ENDOFNAMES => {
                if let Some(name) = msg.param(1) {
                    let name = name.to_vec();
                    let folded = self.support.fold(&name);
                    if let Some(channel) = self.channels.get_mut(&folded) {
                        channel.commit_names(&self.support);
                    }
                    if let Some(channel) = self.channels.get(&folded) {
                        let members = channel
                            .sorted_members(&self.support)
                            .iter()
                            .map(|m| m.display())
                            .collect();
                        out.emit(Event::Names {
                            channel: name.clone(),
                            members,
                        });
                    }
                    out.emit(Event::RosterChanged { channel: name });
                }
            }
            numeric::RPL_TOPIC => {
                if let Some(name) = msg.param(1) {
                    let topic = msg.params().last().map(|t| (*t).to_vec());
                    let folded = self.support.fold(name);
                    if let Some(channel) = self.channels.get_mut(&folded) {
                        channel.topic.clone_from(&topic);
                    }
                    out.emit(Event::Topic {
                        channel: name.to_vec(),
                        topic,
                        setter: None,
                    });
                }
            }
            numeric::RPL_NOTOPIC => {
                if let Some(name) = msg.param(1) {
                    let folded = self.support.fold(name);
                    if let Some(channel) = self.channels.get_mut(&folded) {
                        channel.topic = None;
                    }
                    out.emit(Event::Topic {
                        channel: name.to_vec(),
                        topic: None,
                        setter: None,
                    });
                }
            }
            numeric::RPL_TOPICWHOTIME => {
                if let (Some(name), Some(setter)) = (msg.param(1), msg.param(2)) {
                    let folded = self.support.fold(name);
                    if let Some(channel) = self.channels.get_mut(&folded) {
                        channel.topic_setter = Some(setter.to_vec());
                    }
                }
            }
            _ => Self::emit_numeric(code, msg, out),
        }
    }

    fn emit_numeric(code: u16, msg: &Message<'_>, out: &mut Outcome) {
        // The first parameter is always our own nickname; a UI does not need
        // to be told its own name on every reply.
        let params: Vec<Vec<u8>> = msg.params().iter().skip(1).map(|p| (*p).to_vec()).collect();
        out.emit(Event::Numeric {
            code,
            params,
            text: msg.params().last().map(|t| (*t).to_vec()),
        });
    }

    fn try_next_nick(&mut self, out: &mut Outcome) {
        let candidate = if let Some(alt) = self.config.alt_nicks.get(self.nick_attempt) {
            alt.clone()
        } else {
            // Alternatives exhausted: append underscores, which is what every
            // client does and what users expect to see.
            //
            // When that would exceed the network's limit, the base is shortened
            // rather than the whole candidate truncated. Truncating the result
            // would cut the underscores back off and propose the same rejected
            // nickname again, forever.
            let extra = self.nick_attempt - self.config.alt_nicks.len() + 1;
            let limit = self.support.nicklen().max(1);
            let underscores = extra.min(limit);
            let base_len = limit
                .saturating_sub(underscores)
                .min(self.config.nick.len());
            let mut candidate = self.config.nick[..base_len].to_vec();
            candidate.extend(std::iter::repeat_n(b'_', underscores));
            candidate
        };
        self.nick_attempt += 1;

        if self.nick_attempt > self.config.alt_nicks.len() + 8 {
            self.phase = Phase::Ended;
            out.emit(Event::Ended(Ended::RegistrationFailed(
                "every nickname was taken".into(),
            )));
            out.actions.push(Action::Disconnect);
            return;
        }
        self.nick.clone_from(&candidate);
        out.send(MessageBuf::new("NICK").param(candidate));
    }

    // --- conversation ------------------------------------------------------

    fn handle_message(&mut self, msg: &Message<'_>, kind: MessageKind, out: &mut Outcome) {
        let (Some(target), Some(text)) = (msg.param(0), msg.param(1)) else {
            return;
        };
        out.emit(Event::Message {
            target: self.target_of(target),
            from: Self::sender_of(msg),
            text: text.to_vec(),
            kind,
            time: tag_value(msg, b"time"),
        });
    }

    fn handle_tagmsg(&mut self, msg: &Message<'_>, out: &mut Outcome) {
        let Some(target) = msg.param(0) else {
            return;
        };
        let tags: Vec<(Vec<u8>, Vec<u8>)> = msg
            .tags()
            .iter()
            .filter(|tag| tag.is_client_only())
            .map(|tag| (tag.key.to_vec(), tag.value().into_owned()))
            .collect();
        if tags.is_empty() {
            return;
        }
        out.emit(Event::TagMessage {
            target: self.target_of(target),
            from: Self::sender_of(msg),
            tags,
        });
    }

    fn handle_join(&mut self, msg: &Message<'_>, out: &mut Outcome) {
        let Some(name) = msg.param(0) else {
            return;
        };
        let who = Self::sender_of(msg);
        let is_self = self.is_me(&who.nick);
        let folded = self.support.fold(name);

        if is_self {
            self.channels
                .entry(folded.clone())
                .or_insert_with(|| Channel::new(name.to_vec()));
        }
        if let Some(channel) = self.channels.get_mut(&folded) {
            let mut member = Member::new(who.nick.clone());
            // `extended-join` puts the account and realname in the join
            // itself, saving a WHOIS for everyone who witnesses it.
            if let Some(account) = msg.param(1).filter(|a| *a != b"*") {
                member.account = Some(account.to_vec());
            }
            channel.add(&self.support, member);
        }

        out.emit(Event::Joined {
            channel: name.to_vec(),
            who,
            is_self,
        });
        out.emit(Event::RosterChanged {
            channel: name.to_vec(),
        });
    }

    fn handle_part(&mut self, msg: &Message<'_>, out: &mut Outcome) {
        let Some(name) = msg.param(0) else {
            return;
        };
        let who = Self::sender_of(msg);
        let is_self = self.is_me(&who.nick);
        let folded = self.support.fold(name);

        if is_self {
            self.channels.remove(&folded);
        } else if let Some(channel) = self.channels.get_mut(&folded) {
            channel.remove(&self.support, &who.nick);
        }

        out.emit(Event::Parted {
            channel: name.to_vec(),
            who,
            reason: msg.param(1).map(<[u8]>::to_vec),
            is_self,
        });
        if !is_self {
            out.emit(Event::RosterChanged {
                channel: name.to_vec(),
            });
        }
    }

    fn handle_quit(&mut self, msg: &Message<'_>, out: &mut Outcome) {
        let who = Self::sender_of(msg);
        let mut affected = Vec::new();
        for channel in self.channels.values_mut() {
            if channel.remove(&self.support, &who.nick).is_some() {
                affected.push(channel.name.clone());
            }
        }
        for channel in &affected {
            out.emit(Event::RosterChanged {
                channel: channel.clone(),
            });
        }
        out.emit(Event::Quit {
            who,
            reason: msg.param(0).map(<[u8]>::to_vec),
            channels: affected,
        });
    }

    fn handle_kick(&mut self, msg: &Message<'_>, out: &mut Outcome) {
        let (Some(name), Some(victim)) = (msg.param(0), msg.param(1)) else {
            return;
        };
        let by = Self::sender_of(msg);
        let is_self = self.is_me(victim);
        let folded = self.support.fold(name);

        if is_self {
            self.channels.remove(&folded);
        } else if let Some(channel) = self.channels.get_mut(&folded) {
            channel.remove(&self.support, victim);
        }

        out.emit(Event::Kicked {
            channel: name.to_vec(),
            who: victim.to_vec(),
            by,
            reason: msg.param(2).map(<[u8]>::to_vec),
            is_self,
        });
        if !is_self {
            out.emit(Event::RosterChanged {
                channel: name.to_vec(),
            });
        }
    }

    fn handle_nick(&mut self, msg: &Message<'_>, out: &mut Outcome) {
        let Some(new) = msg.param(0) else {
            return;
        };
        let who = Self::sender_of(msg);
        let is_self = self.is_me(&who.nick);

        let mut affected = Vec::new();
        for channel in self.channels.values_mut() {
            if channel.rename(&self.support, &who.nick, new) {
                affected.push(channel.name.clone());
            }
        }
        if is_self {
            self.nick = new.to_vec();
        }

        out.emit(Event::NickChanged {
            old: who.nick.clone(),
            new: new.to_vec(),
            channels: affected.clone(),
            is_self,
        });
        for channel in affected {
            out.emit(Event::RosterChanged { channel });
        }
    }

    fn handle_mode(&mut self, msg: &Message<'_>, out: &mut Outcome) {
        let (Some(target), Some(spec)) = (msg.param(0), msg.param(1)) else {
            return;
        };
        let params: Vec<Vec<u8>> = msg.params().iter().skip(2).map(|p| (*p).to_vec()).collect();

        if self.support.is_channel(target) {
            self.apply_channel_modes(target, spec, &params, out);
        }
        out.emit(Event::ModeChanged {
            target: target.to_vec(),
            spec: spec.to_vec(),
            params,
            by: Self::sender_of(msg),
        });
    }

    /// Track the membership prefixes a `MODE` grants or removes.
    fn apply_channel_modes(
        &mut self,
        target: &[u8],
        spec: &[u8],
        params: &[Vec<u8>],
        out: &mut Outcome,
    ) {
        let folded = self.support.fold(target);
        // Which letters take a parameter, so the right nickname pairs with
        // the right mode. Getting this wrong grants ops to the wrong person.
        let prefixes: Vec<(u8, u8)> = self
            .support
            .prefixes()
            .iter()
            .map(|p| (p.mode, p.symbol))
            .collect();
        let type_a = self.support.chanmodes(0).to_vec();
        let type_b = self.support.chanmodes(1).to_vec();
        let type_c = self.support.chanmodes(2).to_vec();

        let mut adding = true;
        let mut next = params.iter();
        let mut changed = false;

        for &letter in spec {
            match letter {
                b'+' => adding = true,
                b'-' => adding = false,
                _ => {
                    let is_prefix = prefixes.iter().any(|(mode, _)| *mode == letter);
                    let takes_param = is_prefix
                        || type_a.contains(&letter)
                        || type_b.contains(&letter)
                        || (adding && type_c.contains(&letter));

                    let param = if takes_param { next.next() } else { None };
                    if !is_prefix {
                        continue;
                    }
                    let (Some(nick), Some((_, symbol))) =
                        (param, prefixes.iter().find(|(mode, _)| *mode == letter))
                    else {
                        continue;
                    };
                    if let Some(channel) = self.channels.get_mut(&folded)
                        && channel.set_prefix(&self.support, nick, *symbol, adding)
                    {
                        changed = true;
                    }
                }
            }
        }
        if changed {
            out.emit(Event::RosterChanged {
                channel: target.to_vec(),
            });
        }
    }

    fn handle_topic(&mut self, msg: &Message<'_>, out: &mut Outcome) {
        let Some(name) = msg.param(0) else {
            return;
        };
        let topic = msg.param(1).filter(|t| !t.is_empty()).map(<[u8]>::to_vec);
        let who = Self::sender_of(msg);
        let folded = self.support.fold(name);

        if let Some(channel) = self.channels.get_mut(&folded) {
            channel.topic.clone_from(&topic);
            channel.topic_setter = Some(who.nick.clone());
        }
        out.emit(Event::Topic {
            channel: name.to_vec(),
            topic,
            setter: Some(who.nick),
        });
    }

    fn handle_away(&mut self, msg: &Message<'_>, out: &mut Outcome) {
        let who = Self::sender_of(msg);
        let message = msg.param(0).filter(|m| !m.is_empty()).map(<[u8]>::to_vec);
        let away = message.is_some();
        for channel in self.channels.values_mut() {
            channel.set_away(&self.support, &who.nick, away);
        }
        out.emit(Event::AwayChanged {
            nick: who.nick,
            message,
        });
    }

    fn handle_account(&mut self, msg: &Message<'_>, out: &mut Outcome) {
        let who = Self::sender_of(msg);
        // `*` means logged out.
        let account = msg.param(0).filter(|a| *a != b"*").map(<[u8]>::to_vec);
        for channel in self.channels.values_mut() {
            channel.set_account(&self.support, &who.nick, account.clone());
        }
        if self.is_me(&who.nick) {
            self.account = account;
        }
        out.emit(Event::Raw(MessageBuf::from(msg)));
    }

    fn handle_invite(msg: &Message<'_>, out: &mut Outcome) {
        let Some(channel) = msg.param(1) else {
            return;
        };
        out.emit(Event::Invited {
            channel: channel.to_vec(),
            by: Self::sender_of(msg),
        });
    }

    fn handle_standard_reply(severity: &[u8], msg: &Message<'_>, out: &mut Outcome) {
        // <severity> <command> [code] [context...] :<description>
        let Some(command) = msg.param(0) else {
            return;
        };
        let text = msg.params().last().copied().unwrap_or(b"");
        // The code is only a code when there is something after it; with two
        // parameters the second is the description.
        let code = if msg.params().len() > 2 {
            msg.param(1).unwrap_or(b"")
        } else {
            b""
        };
        out.emit(Event::StandardReply {
            severity: severity.to_vec(),
            command: command.to_vec(),
            code: code.to_vec(),
            text: text.to_vec(),
        });
    }

    fn handle_call(msg: &Message<'_>, out: &mut Outcome) {
        // CALL <call-id> <verb> [params...]
        let (Some(call_id), Some(verb)) = (msg.param(0), msg.param(1)) else {
            return;
        };
        out.emit(Event::Call {
            call_id: call_id.to_vec(),
            verb: verb.to_ascii_uppercase(),
            from: Self::sender_of(msg),
            params: msg.params().iter().skip(2).map(|p| (*p).to_vec()).collect(),
        });
    }

    fn handle_names(&mut self, msg: &Message<'_>) {
        // RPL_NAMREPLY: <me> <symbol> <channel> :<names>
        let (Some(name), Some(names)) = (msg.param(2), msg.params().last()) else {
            return;
        };
        let folded = self.support.fold(name);
        let entries: Vec<Member> = names
            .split(|&b| b == b' ')
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                let (prefixes, rest) = self.support.split_prefixes(entry);
                // `userhost-in-names` appends the full mask; the nickname is
                // everything before the `!`.
                let nick = rest
                    .iter()
                    .position(|&b| b == b'!')
                    .map_or(rest, |i| &rest[..i]);
                Member {
                    nick: nick.to_vec(),
                    prefixes,
                    account: None,
                    away: false,
                }
            })
            .collect();

        let channel = self
            .channels
            .entry(folded)
            .or_insert_with(|| Channel::new(name.to_vec()));
        for member in entries {
            channel.push_name(member);
        }
    }

    // --- helpers -----------------------------------------------------------

    fn target_of(&self, raw: &[u8]) -> Target {
        if self.support.is_channel(raw) {
            Target::Channel(raw.to_vec())
        } else {
            Target::Direct(raw.to_vec())
        }
    }

    fn sender_of(msg: &Message<'_>) -> Sender {
        match msg.source() {
            Some(source) => Sender {
                nick: source.nick.to_vec(),
                mask: Some(source.raw.to_vec()),
                account: tag_value(msg, b"account").map(String::into_bytes),
                is_server: source.is_server(),
            },
            None => Sender {
                nick: Vec::new(),
                mask: None,
                account: None,
                is_server: true,
            },
        }
    }
}

/// Split a space-separated list into owned words.
fn split_words(list: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(list)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn trailing_text(msg: &Message<'_>) -> Option<String> {
    msg.params().last().map(|t| lossy(t))
}

fn tag_value(msg: &Message<'_>, key: &[u8]) -> Option<String> {
    msg.tag(key).map(|tag| lossy(&tag.value()))
}

/// Split an encoded SASL response into chunks the server can reassemble.
///
/// A response whose length is an exact multiple of the chunk size needs a
/// trailing `+`, or the server waits forever for a short chunk that never
/// arrives. An empty response is a bare `+`.
fn chunk_payload(encoded: &str) -> Vec<String> {
    if encoded.is_empty() {
        return vec!["+".to_owned()];
    }
    let mut chunks: Vec<String> = encoded
        .as_bytes()
        .chunks(MAX_SASL_CHUNK)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect();
    if encoded.len().is_multiple_of(MAX_SASL_CHUNK) {
        chunks.push("+".to_owned());
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::{MAX_SASL_CHUNK, chunk_payload};

    #[test]
    fn an_empty_sasl_response_is_a_bare_plus() {
        assert_eq!(chunk_payload(""), vec!["+"]);
    }

    #[test]
    fn a_short_response_is_one_chunk() {
        assert_eq!(chunk_payload("abc"), vec!["abc"]);
    }

    #[test]
    fn an_exact_multiple_gets_a_trailing_plus() {
        // Without it the server waits for a short chunk that never comes.
        let payload = "x".repeat(MAX_SASL_CHUNK);
        let chunks = chunk_payload(&payload);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1], "+");
    }

    #[test]
    fn a_long_response_splits_at_the_chunk_size() {
        let payload = "x".repeat(MAX_SASL_CHUNK + 10);
        let chunks = chunk_payload(&payload);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), MAX_SASL_CHUNK);
        assert_eq!(chunks[1].len(), 10);
    }
}
