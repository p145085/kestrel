//! Compact session descriptions.
//!
//! A WebRTC offer is 3–5 KB of SDP, almost all of it boilerplate that both
//! ends already know. Sending that through a chat protocol, in a burst, once
//! per peer, is what would make calls feel broken on a busy network.
//!
//! Both ends run the same client, so they share a table of codec profiles and
//! exchange only what genuinely varies: the fingerprint, the ICE credentials,
//! the setup role, which profile, and the stream identifiers. That is about
//! 200 bytes, and each side reconstitutes full SDP from it locally.

use serde::{Deserialize, Serialize};

/// Version of the compact format.
pub const CSD_VERSION: u8 = 0;

/// Which end sets up the DTLS association.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Setup {
    /// Willing to be either; the offerer says this.
    ActPass,
    /// This end opens the connection.
    Active,
    /// This end waits.
    Passive,
}

impl Setup {
    /// The SDP attribute value.
    #[must_use]
    pub fn as_sdp(self) -> &'static str {
        match self {
            Self::ActPass => "actpass",
            Self::Active => "active",
            Self::Passive => "passive",
        }
    }

    /// Parse an SDP attribute value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "actpass" => Some(Self::ActPass),
            "active" => Some(Self::Active),
            "passive" => Some(Self::Passive),
            _ => None,
        }
    }

    /// The role the other end must take against this one.
    #[must_use]
    pub fn answering(self) -> Self {
        match self {
            // Answering `actpass` means choosing; the answerer takes active,
            // which is what every implementation does.
            Self::ActPass | Self::Passive => Self::Active,
            Self::Active => Self::Passive,
        }
    }
}

/// A codec profile both ends understand by number.
///
/// The number is the whole point: it stands in for every `a=rtpmap`,
/// `a=fmtp` and `a=extmap` line the profile implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Profile {
    /// Opus audio and VP8 video. Universally interoperable, no patent
    /// exposure, and the fallback when a peer offers something unknown.
    OpusVp8,
    /// Opus audio and VP9 video.
    OpusVp9,
    /// Opus audio only.
    OpusOnly,
}

impl Profile {
    /// The wire number.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            Self::OpusVp8 => 0,
            Self::OpusVp9 => 1,
            Self::OpusOnly => 2,
        }
    }

    /// Parse a wire number.
    ///
    /// An unknown profile falls back to [`Profile::OpusVp8`] rather than
    /// failing: a future client offering something we do not have should still
    /// get a call, using the profile everyone supports.
    #[must_use]
    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::OpusVp9,
            2 => Self::OpusOnly,
            _ => Self::OpusVp8,
        }
    }

    /// Whether this profile carries video.
    #[must_use]
    pub fn has_video(self) -> bool {
        !matches!(self, Self::OpusOnly)
    }

    /// The video codec's name and payload type, if it has one.
    #[must_use]
    pub fn video_codec(self) -> Option<(&'static str, u8)> {
        match self {
            Self::OpusVp8 => Some(("VP8", 96)),
            Self::OpusVp9 => Some(("VP9", 98)),
            Self::OpusOnly => None,
        }
    }
}

/// Everything about one end of a call that the other end cannot infer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDescription {
    /// Format version.
    #[serde(rename = "v")]
    pub version: u8,
    /// SHA-256 fingerprint of the DTLS certificate.
    ///
    /// This is the value a short authentication string ultimately protects:
    /// substituting it is how a hostile relay would put itself in the middle.
    #[serde(rename = "f", with = "serde_bytes_array")]
    pub fingerprint: [u8; 32],
    /// ICE username fragment.
    #[serde(rename = "u")]
    pub ice_ufrag: String,
    /// ICE password.
    #[serde(rename = "p")]
    pub ice_pwd: String,
    /// DTLS setup role.
    #[serde(rename = "s")]
    pub setup: Setup,
    /// Codec profile.
    #[serde(rename = "c")]
    pub profile: Profile,
    /// Audio stream identifier.
    #[serde(rename = "a")]
    pub audio_ssrc: u32,
    /// Video stream identifier, when the profile carries video.
    #[serde(rename = "w")]
    pub video_ssrc: Option<u32>,
}

impl SessionDescription {
    /// A description with the current version.
    #[must_use]
    pub fn new(
        fingerprint: [u8; 32],
        ice_ufrag: impl Into<String>,
        ice_pwd: impl Into<String>,
        setup: Setup,
        profile: Profile,
        audio_ssrc: u32,
        video_ssrc: Option<u32>,
    ) -> Self {
        Self {
            version: CSD_VERSION,
            fingerprint,
            ice_ufrag: ice_ufrag.into(),
            ice_pwd: ice_pwd.into(),
            setup,
            profile,
            audio_ssrc,
            // A profile with no video must carry no video stream, or the
            // reconstituted SDP would describe a stream nothing will send.
            video_ssrc: if profile.has_video() {
                video_ssrc
            } else {
                None
            },
        }
    }

    /// The fingerprint as SDP writes it: uppercase hex, colon-separated.
    #[must_use]
    pub fn fingerprint_sdp(&self) -> String {
        use std::fmt::Write;
        let mut out = String::with_capacity(32 * 3);
        for (index, byte) in self.fingerprint.iter().enumerate() {
            if index > 0 {
                out.push(':');
            }
            let _ = write!(out, "{byte:02X}");
        }
        out
    }

    /// Parse a fingerprint in SDP's colon-separated hex form.
    #[must_use]
    pub fn parse_fingerprint(text: &str) -> Option<[u8; 32]> {
        let mut out = [0u8; 32];
        let mut parts = text.split(':');
        for slot in &mut out {
            *slot = u8::from_str_radix(parts.next()?, 16).ok()?;
        }
        // More than 32 octets is not a SHA-256 fingerprint.
        parts.next().is_none().then_some(out)
    }
}

/// How a candidate was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateKind {
    /// An address on this machine. Discloses the local network's shape.
    Host,
    /// The public address a STUN server saw. Discloses the real IP.
    ServerReflexive,
    /// A relay. Discloses only the relay.
    Relay,
    /// Learned from an incoming connectivity check.
    PeerReflexive,
}

impl CandidateKind {
    /// The SDP token.
    #[must_use]
    pub fn as_sdp(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::ServerReflexive => "srflx",
            Self::Relay => "relay",
            Self::PeerReflexive => "prflx",
        }
    }

    /// Parse an SDP token.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "host" => Some(Self::Host),
            "srflx" => Some(Self::ServerReflexive),
            "relay" => Some(Self::Relay),
            "prflx" => Some(Self::PeerReflexive),
            _ => None,
        }
    }

    /// Whether sending this candidate discloses the sender's own address.
    ///
    /// The basis of relay-only mode: a client that has promised not to reveal
    /// an address must send only candidates for which this is false.
    #[must_use]
    pub fn discloses_address(self) -> bool {
        !matches!(self, Self::Relay)
    }
}

/// One ICE candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// Groups candidates that share a base.
    #[serde(rename = "f")]
    pub foundation: String,
    /// 1 for RTP, 2 for RTCP.
    #[serde(rename = "c")]
    pub component: u8,
    /// `udp` or `tcp`.
    #[serde(rename = "t")]
    pub transport: String,
    /// ICE priority.
    #[serde(rename = "p")]
    pub priority: u32,
    /// The address, as text so that both IPv4 and `.local` names fit.
    #[serde(rename = "a")]
    pub address: String,
    /// The port.
    #[serde(rename = "o")]
    pub port: u16,
    /// How it was discovered.
    #[serde(rename = "k")]
    pub kind: CandidateKind,
    /// Which media section it belongs to.
    #[serde(rename = "m")]
    pub mid: String,
}

impl Candidate {
    /// Render as the value of an SDP `a=candidate:` attribute.
    #[must_use]
    pub fn to_sdp(&self) -> String {
        format!(
            "candidate:{} {} {} {} {} {} typ {}",
            self.foundation,
            self.component,
            self.transport,
            self.priority,
            self.address,
            self.port,
            self.kind.as_sdp()
        )
    }

    /// Parse an SDP `a=candidate:` value.
    #[must_use]
    pub fn parse_sdp(line: &str, mid: impl Into<String>) -> Option<Self> {
        let rest = line
            .trim()
            .strip_prefix("a=")
            .unwrap_or(line.trim())
            .strip_prefix("candidate:")?;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // foundation component transport priority address port typ kind
        if fields.len() < 8 || fields[6] != "typ" {
            return None;
        }
        Some(Self {
            foundation: fields[0].to_owned(),
            component: fields[1].parse().ok()?,
            transport: fields[2].to_owned(),
            priority: fields[3].parse().ok()?,
            address: fields[4].to_owned(),
            port: fields[5].parse().ok()?,
            kind: CandidateKind::parse(fields[7])?,
            mid: mid.into(),
        })
    }
}

/// Carry a fixed-size byte array as a CBOR byte string.
///
/// Serde derives for arrays only up to 32 elements, and CBOR would otherwise
/// encode one as an array of integers — roughly twice the bytes for no gain.
pub mod serde_bytes_array {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Write the array as a byte string.
    pub fn serialize<S: Serializer, const N: usize>(
        bytes: &[u8; N],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(bytes)
    }

    /// Read a byte string of exactly the expected length.
    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<[u8; N], D::Error> {
        let bytes = serde_bytes::ByteBuf::deserialize(d)?;
        <[u8; N]>::try_from(bytes.as_ref())
            .map_err(|_| D::Error::custom(format!("expected {N} bytes")))
    }
}

#[cfg(test)]
mod tests {
    use super::{Candidate, CandidateKind, Profile, SessionDescription, Setup};

    fn description() -> SessionDescription {
        SessionDescription::new(
            [0xab; 32],
            "ufrag",
            "a-longer-ice-password",
            Setup::ActPass,
            Profile::OpusVp8,
            1111,
            Some(2222),
        )
    }

    #[test]
    fn setup_roles_round_trip() {
        for setup in [Setup::ActPass, Setup::Active, Setup::Passive] {
            assert_eq!(Setup::parse(setup.as_sdp()), Some(setup));
        }
    }

    #[test]
    fn answering_actpass_takes_the_active_role() {
        assert_eq!(Setup::ActPass.answering(), Setup::Active);
        assert_eq!(Setup::Passive.answering(), Setup::Active);
        assert_eq!(Setup::Active.answering(), Setup::Passive);
    }

    #[test]
    fn an_unknown_profile_falls_back_to_the_universal_one() {
        // A future client offering something we do not have should still get a
        // call rather than nothing.
        assert_eq!(Profile::from_u8(200), Profile::OpusVp8);
        for profile in [Profile::OpusVp8, Profile::OpusVp9, Profile::OpusOnly] {
            assert_eq!(Profile::from_u8(profile.as_u8()), profile);
        }
    }

    #[test]
    fn an_audio_only_profile_carries_no_video_stream() {
        // Otherwise the reconstituted SDP would describe a stream nothing sends.
        let audio_only = SessionDescription::new(
            [0; 32],
            "u",
            "p",
            Setup::Active,
            Profile::OpusOnly,
            1111,
            Some(2222),
        );
        assert!(audio_only.video_ssrc.is_none());
        assert!(!Profile::OpusOnly.has_video());
    }

    #[test]
    fn fingerprints_round_trip_through_their_sdp_form() {
        let description = description();
        let text = description.fingerprint_sdp();
        assert!(text.starts_with("AB:AB:"), "got {text}");
        assert_eq!(
            SessionDescription::parse_fingerprint(&text),
            Some(description.fingerprint)
        );
    }

    #[test]
    fn a_malformed_fingerprint_is_refused() {
        for bad in ["", "AB", "AB:AB", "not:hex:at:all", &"AB:".repeat(40)] {
            assert!(
                SessionDescription::parse_fingerprint(bad).is_none(),
                "should refuse {bad}"
            );
        }
    }

    #[test]
    fn candidates_round_trip_through_sdp() {
        let candidate = Candidate {
            foundation: "1".to_owned(),
            component: 1,
            transport: "udp".to_owned(),
            priority: 2_130_706_431,
            address: "192.168.1.100".to_owned(),
            port: 54321,
            kind: CandidateKind::Host,
            mid: "0".to_owned(),
        };
        let parsed = Candidate::parse_sdp(&candidate.to_sdp(), "0").unwrap();
        assert_eq!(parsed, candidate);
    }

    #[test]
    fn candidate_lines_are_accepted_with_or_without_the_attribute_prefix() {
        let line = "a=candidate:1 1 udp 2130706431 10.0.0.1 9000 typ srflx";
        assert!(Candidate::parse_sdp(line, "0").is_some());
        assert!(Candidate::parse_sdp(line.trim_start_matches("a="), "0").is_some());
    }

    #[test]
    fn a_malformed_candidate_is_refused() {
        for bad in [
            "candidate:1 1 udp",
            "candidate:1 1 udp 100 10.0.0.1 9000 xyz host",
            "candidate:1 1 udp 100 10.0.0.1 notaport typ host",
            "not a candidate",
        ] {
            assert!(
                Candidate::parse_sdp(bad, "0").is_none(),
                "should refuse {bad}"
            );
        }
    }

    #[test]
    fn only_relay_candidates_hide_the_senders_address() {
        // This is what relay-only mode filters on.
        assert!(!CandidateKind::Relay.discloses_address());
        for kind in [
            CandidateKind::Host,
            CandidateKind::ServerReflexive,
            CandidateKind::PeerReflexive,
        ] {
            assert!(kind.discloses_address(), "{kind:?} reveals an address");
        }
    }
}
