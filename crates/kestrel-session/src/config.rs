//! How a session should present itself and what it should negotiate.

/// Credentials for SASL authentication.
#[derive(Clone, PartialEq, Eq)]
pub enum Sasl {
    /// Account name and password.
    Plain {
        /// The account to authenticate as.
        account: Vec<u8>,
        /// The password.
        password: Vec<u8>,
    },
    /// Authenticate with the TLS client certificate already presented.
    External,
}

impl std::fmt::Debug for Sasl {
    /// Redacts the password.
    ///
    /// Session configuration ends up in logs and crash reports, and a password
    /// that reaches either has to be treated as disclosed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plain { account, .. } => f
                .debug_struct("Plain")
                .field("account", &String::from_utf8_lossy(account))
                .field("password", &"<redacted>")
                .finish(),
            Self::External => f.write_str("External"),
        }
    }
}

/// Everything a session needs to register and settle in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    /// Preferred nickname.
    pub nick: Vec<u8>,
    /// Fallbacks to try when the preferred nickname is taken.
    pub alt_nicks: Vec<Vec<u8>>,
    /// Username sent in `USER`; servers usually prefix it with `~`.
    pub username: Vec<u8>,
    /// Realname sent in `USER`.
    pub realname: Vec<u8>,
    /// Password sent with `PASS`, for servers that require one.
    pub server_password: Option<Vec<u8>>,
    /// SASL credentials, if authenticating.
    pub sasl: Option<Sasl>,
    /// Capabilities to request when the server offers them.
    pub wanted_caps: Vec<String>,
    /// Channels to join once registered.
    pub autojoin: Vec<Vec<u8>>,
}

/// Capabilities worth having in any client.
///
/// Every one of these changes what the client can show without changing what
/// the user has to do: timestamps that survive a bouncer replay, the account
/// behind a nickname, a join that already carries the realname.
pub const DEFAULT_CAPS: &[&str] = &[
    "account-notify",
    "account-tag",
    "away-notify",
    "batch",
    "cap-notify",
    "chghost",
    "echo-message",
    "extended-join",
    "invite-notify",
    "message-tags",
    "multi-prefix",
    "sasl",
    "server-time",
    "setname",
    "standard-replies",
    "userhost-in-names",
];

impl SessionConfig {
    /// A configuration with the given nickname and sensible defaults.
    #[must_use]
    pub fn new(nick: impl Into<Vec<u8>>) -> Self {
        let nick = nick.into();
        Self {
            username: nick.clone(),
            realname: nick.clone(),
            nick,
            alt_nicks: Vec::new(),
            server_password: None,
            sasl: None,
            wanted_caps: DEFAULT_CAPS.iter().map(|c| (*c).to_owned()).collect(),
            autojoin: Vec::new(),
        }
    }

    /// Set the realname.
    #[must_use]
    pub fn with_realname(mut self, realname: impl Into<Vec<u8>>) -> Self {
        self.realname = realname.into();
        self
    }

    /// Set the username.
    #[must_use]
    pub fn with_username(mut self, username: impl Into<Vec<u8>>) -> Self {
        self.username = username.into();
        self
    }

    /// Authenticate with SASL.
    #[must_use]
    pub fn with_sasl(mut self, sasl: Sasl) -> Self {
        self.sasl = Some(sasl);
        self
    }

    /// Join these channels once registered.
    #[must_use]
    pub fn with_autojoin<I, C>(mut self, channels: I) -> Self
    where
        I: IntoIterator<Item = C>,
        C: Into<Vec<u8>>,
    {
        self.autojoin = channels.into_iter().map(Into::into).collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{Sasl, SessionConfig};

    #[test]
    fn a_new_config_reuses_the_nick_for_user_fields() {
        let config = SessionConfig::new("alice");
        assert_eq!(config.nick, b"alice");
        assert_eq!(config.username, b"alice");
        assert_eq!(config.realname, b"alice");
    }

    #[test]
    fn sasl_passwords_do_not_appear_in_debug_output() {
        // Configuration reaches logs and crash reports; a password that gets
        // there has to be treated as disclosed.
        let sasl = Sasl::Plain {
            account: b"alice".to_vec(),
            password: b"hunter2".to_vec(),
        };
        let printed = format!("{sasl:?}");
        assert!(!printed.contains("hunter2"), "got {printed}");
        assert!(printed.contains("redacted"), "got {printed}");
        assert!(printed.contains("alice"), "the account is not secret");
    }

    #[test]
    fn a_whole_config_redacts_its_password_too() {
        let config = SessionConfig::new("alice").with_sasl(Sasl::Plain {
            account: b"alice".to_vec(),
            password: b"hunter2".to_vec(),
        });
        assert!(!format!("{config:?}").contains("hunter2"));
    }

    #[test]
    fn builders_set_what_they_say() {
        let config = SessionConfig::new("alice")
            .with_realname("Alice Liddell")
            .with_username("liddell")
            .with_autojoin(["#one", "#two"]);
        assert_eq!(config.realname, b"Alice Liddell");
        assert_eq!(config.username, b"liddell");
        assert_eq!(config.autojoin, vec![b"#one".to_vec(), b"#two".to_vec()]);
    }
}
