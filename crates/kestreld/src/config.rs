//! Configuration file handling.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use kestrel_proto::CaseMapping;
use kestreld_core::ServerConfig;
use serde::{Deserialize, Serialize};

/// The on-disk configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Addresses to accept plaintext connections on.
    pub listen: Vec<SocketAddr>,
    /// Addresses to accept TLS connections on.
    ///
    /// Requires `tls_certificate` and `tls_key`.
    pub tls_listen: Vec<SocketAddr>,
    /// PEM file holding the server's certificate chain.
    pub tls_certificate: Option<PathBuf>,
    /// PEM file holding the server's private key.
    pub tls_key: Option<PathBuf>,
    /// The server's own name.
    pub server_name: String,
    /// Network name, advertised as the `NETWORK` token.
    pub network_name: String,
    /// Message of the day, one entry per line.
    pub motd: Vec<String>,
    /// A password every client must supply via `PASS`.
    pub password: Option<String>,
    /// Name folding rule: `rfc1459`, `rfc1459-strict` or `ascii`.
    pub casemapping: String,
    /// Longest permitted nickname.
    pub max_nick_len: usize,
    /// Longest permitted channel name.
    pub max_channel_len: usize,
    /// How many channels one client may be in at once.
    pub max_channels_per_client: usize,
    /// Seconds a connection may stay silent before it is closed.
    pub idle_timeout_secs: u64,
    /// Seconds a connection may take to finish registering.
    pub registration_timeout_secs: u64,
    /// Where registered accounts are saved. Unset means they are not
    /// persisted and vanish when the server stops.
    pub accounts_file: Option<PathBuf>,
    /// Accounts to create at start-up, for bootstrapping a new server.
    pub accounts: Vec<AccountConfig>,
}

/// An account declared in the configuration file.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    /// The account name.
    pub name: String,
    /// The password, in the clear.
    ///
    /// Hashed with Argon2 at start-up and never stored in that form, but it is
    /// still a plaintext password sitting in a file — suitable for getting a
    /// server going, not for running one.
    pub password: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: vec!["127.0.0.1:6667".parse().expect("valid default address")],
            tls_listen: Vec::new(),
            tls_certificate: None,
            tls_key: None,
            server_name: "kestrel.local".to_owned(),
            network_name: "Kestrel".to_owned(),
            motd: Vec::new(),
            password: None,
            casemapping: "rfc1459".to_owned(),
            max_nick_len: 32,
            max_channel_len: 64,
            max_channels_per_client: 128,
            idle_timeout_secs: 300,
            registration_timeout_secs: 60,
            accounts_file: None,
            accounts: Vec::new(),
        }
    }
}

impl Config {
    /// Read a configuration file.
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Turn this into the state machine's configuration.
    #[must_use]
    pub fn to_server_config(&self) -> ServerConfig {
        ServerConfig {
            server_name: self.server_name.clone().into_bytes(),
            network_name: self.network_name.clone().into_bytes(),
            version: format!("kestreld-{}", env!("CARGO_PKG_VERSION")).into_bytes(),
            motd: self.motd.iter().map(|l| l.clone().into_bytes()).collect(),
            casemapping: CaseMapping::parse(self.casemapping.as_bytes()),
            max_nick_len: self.max_nick_len,
            max_channel_len: self.max_channel_len,
            max_channels_per_client: self.max_channels_per_client,
            password: self.password.clone().map(String::into_bytes),
            ..ServerConfig::default()
        }
    }

    /// The TLS certificate and key, if TLS is configured.
    ///
    /// Returns an error rather than silently falling back to plaintext: an
    /// operator who asked for TLS and got a cleartext port instead would have
    /// no way of noticing until it mattered.
    pub fn tls_paths(&self) -> Result<Option<(&Path, &Path)>> {
        if self.tls_listen.is_empty() {
            return Ok(None);
        }
        match (&self.tls_certificate, &self.tls_key) {
            (Some(certificate), Some(key)) => Ok(Some((certificate.as_path(), key.as_path()))),
            _ => anyhow::bail!(
                "tls_listen is set but tls_certificate and tls_key are not both configured"
            ),
        }
    }

    /// The configuration written out as TOML, for `--print-config`.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).context("serialising configuration")
    }
}

#[cfg(test)]
mod tests {
    use super::Config;
    use kestrel_proto::CaseMapping;

    #[test]
    fn the_default_configuration_round_trips_through_toml() {
        let original = Config::default();
        let text = original.to_toml().unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.server_name, original.server_name);
        assert_eq!(parsed.listen, original.listen);
    }

    #[test]
    fn a_minimal_file_fills_in_defaults() {
        let parsed: Config = toml::from_str(r#"server_name = "irc.example.org""#).unwrap();
        assert_eq!(parsed.server_name, "irc.example.org");
        assert_eq!(parsed.network_name, "Kestrel");
        assert_eq!(parsed.max_nick_len, 32);
    }

    #[test]
    fn an_unknown_key_is_an_error_rather_than_being_ignored() {
        // A silently ignored typo in a config file is how a server ends up
        // running with a setting the operator believes they changed.
        let result: Result<Config, _> = toml::from_str(r#"servername = "typo""#);
        assert!(result.is_err());
    }

    #[test]
    fn casemapping_is_translated_for_the_state_machine() {
        let parsed: Config = toml::from_str(r#"casemapping = "ascii""#).unwrap();
        assert_eq!(parsed.to_server_config().casemapping, CaseMapping::Ascii);
    }

    #[test]
    fn tls_without_a_certificate_is_an_error() {
        // Falling back to plaintext here would hand an operator a cleartext
        // port they believed was encrypted.
        let parsed: Config = toml::from_str(r#"tls_listen = ["127.0.0.1:6697"]"#).unwrap();
        assert!(parsed.tls_paths().is_err());
    }

    #[test]
    fn tls_is_off_unless_a_listener_is_configured() {
        assert!(Config::default().tls_paths().unwrap().is_none());
    }

    #[test]
    fn limits_reach_the_state_machine() {
        let parsed: Config = toml::from_str("max_nick_len = 9\nmax_channel_len = 50").unwrap();
        let server = parsed.to_server_config();
        assert_eq!(server.max_nick_len, 9);
        assert_eq!(server.max_channel_len, 50);
    }
}
