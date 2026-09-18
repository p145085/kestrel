//! Reconstituting SDP from a compact session description, and back.
//!
//! This is the other half of the compact format: the boilerplate that was
//! never sent gets written locally from the profile number. Both ends run the
//! same table, so both produce the same session out of the same 200 bytes.

use std::fmt::Write;

use crate::csd::{Candidate, Profile, SessionDescription, Setup};

/// Media section identifiers, in the order the generated SDP uses them.
pub const AUDIO_MID: &str = "0";
/// Identifier of the video section.
pub const VIDEO_MID: &str = "1";

/// The Opus payload type. 111 is what every implementation uses in practice.
const OPUS_PT: u8 = 111;

/// Render a session description as SDP.
///
/// No candidates are included: ICE is always trickled, which is what lets a
/// client withhold its address until the call has been accepted.
#[must_use]
pub fn to_sdp(description: &SessionDescription) -> String {
    let fingerprint = description.fingerprint_sdp();
    let mut mids = vec![AUDIO_MID];
    if description.profile.has_video() {
        mids.push(VIDEO_MID);
    }

    let mut sdp = String::with_capacity(1024);
    // The origin carries no hostname and no real address: it would otherwise
    // disclose the machine to everyone in the call before ICE even begins.
    sdp.push_str("v=0\r\n");
    sdp.push_str("o=- 0 0 IN IP4 0.0.0.0\r\n");
    sdp.push_str("s=-\r\n");
    sdp.push_str("t=0 0\r\n");
    let _ = write!(sdp, "a=group:BUNDLE {}\r\n", mids.join(" "));
    sdp.push_str("a=msid-semantic: WMS kestrel\r\n");

    push_audio(&mut sdp, description, &fingerprint);
    if description.profile.has_video() {
        push_video(&mut sdp, description, &fingerprint);
    }
    sdp
}

fn push_common(sdp: &mut String, description: &SessionDescription, fingerprint: &str, mid: &str) {
    sdp.push_str("c=IN IP4 0.0.0.0\r\n");
    let _ = write!(sdp, "a=ice-ufrag:{}\r\n", description.ice_ufrag);
    let _ = write!(sdp, "a=ice-pwd:{}\r\n", description.ice_pwd);
    // Saying so explicitly is what tells the peer no candidates are coming in
    // the description itself.
    sdp.push_str("a=ice-options:trickle\r\n");
    let _ = write!(sdp, "a=fingerprint:sha-256 {fingerprint}\r\n");
    let _ = write!(sdp, "a=setup:{}\r\n", description.setup.as_sdp());
    let _ = write!(sdp, "a=mid:{mid}\r\n");
    sdp.push_str("a=sendrecv\r\n");
    sdp.push_str("a=rtcp-mux\r\n");
}

fn push_audio(sdp: &mut String, description: &SessionDescription, fingerprint: &str) {
    let _ = write!(sdp, "m=audio 9 UDP/TLS/RTP/SAVPF {OPUS_PT}\r\n");
    push_common(sdp, description, fingerprint, AUDIO_MID);
    let _ = write!(sdp, "a=rtpmap:{OPUS_PT} opus/48000/2\r\n");
    // Opus is stereo-capable and benefits from in-band FEC on a lossy link;
    // both are off unless asked for.
    let _ = write!(sdp, "a=fmtp:{OPUS_PT} minptime=10;useinbandfec=1\r\n");
    let _ = write!(sdp, "a=ssrc:{} cname:kestrel\r\n", description.audio_ssrc);
}

fn push_video(sdp: &mut String, description: &SessionDescription, fingerprint: &str) {
    let Some((codec, payload_type)) = description.profile.video_codec() else {
        return;
    };
    let _ = write!(sdp, "m=video 9 UDP/TLS/RTP/SAVPF {payload_type}\r\n");
    push_common(sdp, description, fingerprint, VIDEO_MID);
    let _ = write!(sdp, "a=rtpmap:{payload_type} {codec}/90000\r\n");
    // Keyframe requests and negative acknowledgements: without them a lost
    // packet costs seconds of frozen video rather than milliseconds.
    let _ = write!(sdp, "a=rtcp-fb:{payload_type} nack\r\n");
    let _ = write!(sdp, "a=rtcp-fb:{payload_type} nack pli\r\n");
    let _ = write!(sdp, "a=rtcp-fb:{payload_type} ccm fir\r\n");
    let _ = write!(sdp, "a=rtcp-fb:{payload_type} goog-remb\r\n");
    if let Some(ssrc) = description.video_ssrc {
        let _ = write!(sdp, "a=ssrc:{ssrc} cname:kestrel\r\n");
    }
}

/// Read a compact description back out of SDP.
///
/// Used on the sending side: the media engine produces full SDP, and this
/// reduces it to what actually has to travel.
#[must_use]
pub fn from_sdp(sdp: &str) -> Option<SessionDescription> {
    let mut ice_ufrag = None;
    let mut ice_pwd = None;
    let mut fingerprint = None;
    let mut setup = None;
    let mut audio_ssrc = None;
    let mut video_ssrc = None;
    let mut has_video = false;
    let mut video_codec = None;
    let mut in_video = false;

    for line in sdp.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("m=") {
            in_video = rest.starts_with("video");
            has_video |= in_video;
        } else if let Some(value) = line.strip_prefix("a=ice-ufrag:") {
            ice_ufrag.get_or_insert_with(|| value.to_owned());
        } else if let Some(value) = line.strip_prefix("a=ice-pwd:") {
            ice_pwd.get_or_insert_with(|| value.to_owned());
        } else if let Some(value) = line.strip_prefix("a=fingerprint:sha-256 ") {
            fingerprint = fingerprint.or_else(|| SessionDescription::parse_fingerprint(value));
        } else if let Some(value) = line.strip_prefix("a=setup:") {
            setup = setup.or_else(|| Setup::parse(value));
        } else if let Some(value) = line.strip_prefix("a=rtpmap:") {
            if in_video && video_codec.is_none() {
                video_codec = value
                    .split_whitespace()
                    .nth(1)
                    .and_then(|codec| codec.split('/').next())
                    .map(str::to_ascii_uppercase);
            }
        } else if let Some(value) = line.strip_prefix("a=ssrc:") {
            let parsed = value.split_whitespace().next().and_then(|s| s.parse().ok());
            if in_video {
                video_ssrc = video_ssrc.or(parsed);
            } else {
                audio_ssrc = audio_ssrc.or(parsed);
            }
        }
    }

    let profile = match (has_video, video_codec.as_deref()) {
        (false, _) => Profile::OpusOnly,
        (true, Some("VP9")) => Profile::OpusVp9,
        // Anything else that carries video is described as VP8: it is the one
        // profile every implementation supports, so it is the safe reduction.
        (true, _) => Profile::OpusVp8,
    };

    Some(SessionDescription::new(
        fingerprint?,
        ice_ufrag?,
        ice_pwd?,
        setup?,
        profile,
        audio_ssrc.unwrap_or(0),
        video_ssrc,
    ))
}

/// Pull every candidate out of an SDP body.
#[must_use]
pub fn candidates_from_sdp(sdp: &str) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let mut mid = AUDIO_MID.to_owned();
    for line in sdp.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("a=mid:") {
            value.clone_into(&mut mid);
        } else if line.starts_with("a=candidate:")
            && let Some(candidate) = Candidate::parse_sdp(line, mid.clone())
        {
            candidates.push(candidate);
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::{candidates_from_sdp, from_sdp, to_sdp};
    use crate::csd::{CandidateKind, Profile, SessionDescription, Setup};

    fn description(profile: Profile) -> SessionDescription {
        SessionDescription::new(
            [0x5a; 32],
            "abcd1234",
            "a-much-longer-ice-password",
            Setup::ActPass,
            profile,
            111_111,
            Some(222_222),
        )
    }

    #[test]
    fn generated_sdp_has_the_shape_webrtc_expects() {
        let sdp = to_sdp(&description(Profile::OpusVp8));
        for required in [
            "v=0\r\n",
            "a=group:BUNDLE 0 1\r\n",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 96\r\n",
            "a=rtpmap:111 opus/48000/2\r\n",
            "a=rtpmap:96 VP8/90000\r\n",
            "a=setup:actpass\r\n",
            "a=ice-options:trickle\r\n",
            "a=rtcp-mux\r\n",
        ] {
            assert!(sdp.contains(required), "missing {required:?} in:\n{sdp}");
        }
    }

    #[test]
    fn the_origin_line_discloses_nothing_about_the_machine() {
        // It would otherwise hand every participant an address before ICE has
        // even started, and before anyone consented to that.
        let sdp = to_sdp(&description(Profile::OpusVp8));
        assert!(sdp.contains("o=- 0 0 IN IP4 0.0.0.0\r\n"), "got:\n{sdp}");
        assert!(sdp.contains("c=IN IP4 0.0.0.0\r\n"));
        assert!(!sdp.contains("192.168"), "a real address leaked:\n{sdp}");
    }

    #[test]
    fn generated_sdp_carries_no_candidates() {
        // Trickle is what lets a client withhold its address until accepted.
        let sdp = to_sdp(&description(Profile::OpusVp8));
        assert!(!sdp.contains("a=candidate:"), "got:\n{sdp}");
    }

    #[test]
    fn an_audio_only_profile_has_no_video_section() {
        let sdp = to_sdp(&description(Profile::OpusOnly));
        assert!(!sdp.contains("m=video"), "got:\n{sdp}");
        assert!(sdp.contains("a=group:BUNDLE 0\r\n"), "got:\n{sdp}");
    }

    #[test]
    fn video_asks_for_the_feedback_that_keeps_it_from_freezing() {
        // Without these a lost packet costs seconds of frozen picture.
        let sdp = to_sdp(&description(Profile::OpusVp8));
        for feedback in ["a=rtcp-fb:96 nack pli", "a=rtcp-fb:96 ccm fir"] {
            assert!(sdp.contains(feedback), "missing {feedback}");
        }
    }

    #[test]
    fn a_description_survives_a_trip_through_sdp() {
        for profile in [Profile::OpusVp8, Profile::OpusVp9, Profile::OpusOnly] {
            let original = description(profile);
            let recovered = from_sdp(&to_sdp(&original)).expect("should parse back");
            assert_eq!(recovered, original, "{profile:?} did not survive");
        }
    }

    #[test]
    fn parsing_incomplete_sdp_fails_rather_than_inventing_values() {
        // A description missing a fingerprint or ICE credentials cannot be
        // used, and guessing would produce a call that silently never connects.
        let full = to_sdp(&description(Profile::OpusVp8));
        for drop in ["a=fingerprint:", "a=ice-ufrag:", "a=ice-pwd:", "a=setup:"] {
            let kept: Vec<&str> = full
                .lines()
                .filter(|line| !line.starts_with(drop))
                .collect();
            let stripped = kept.join("\r\n");
            assert!(
                from_sdp(&stripped).is_none(),
                "should refuse SDP with no {drop}"
            );
        }
    }

    #[test]
    fn an_unknown_video_codec_reduces_to_the_universal_profile() {
        let sdp = to_sdp(&description(Profile::OpusVp8)).replace("VP8/90000", "AV1/90000");
        assert_eq!(from_sdp(&sdp).unwrap().profile, Profile::OpusVp8);
    }

    #[test]
    fn candidates_are_pulled_out_with_the_section_they_belong_to() {
        let sdp = "\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:0\r\n\
a=candidate:1 1 udp 2130706431 10.0.0.1 9000 typ host\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=mid:1\r\n\
a=candidate:2 1 udp 1694498815 203.0.113.9 9001 typ srflx\r\n";

        let candidates = candidates_from_sdp(sdp);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].mid, "0");
        assert_eq!(candidates[0].kind, CandidateKind::Host);
        assert_eq!(candidates[1].mid, "1");
        assert_eq!(candidates[1].kind, CandidateKind::ServerReflexive);
    }

    #[test]
    fn a_malformed_candidate_line_is_skipped_not_fatal() {
        let sdp =
            "a=mid:0\r\na=candidate:broken\r\na=candidate:1 1 udp 100 10.0.0.1 9000 typ host\r\n";
        assert_eq!(candidates_from_sdp(sdp).len(), 1);
    }

    #[test]
    fn sdp_uses_crlf_line_endings_throughout() {
        // Bare newlines are a real source of interoperability failures.
        let sdp = to_sdp(&description(Profile::OpusVp8));
        for line in sdp.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(!line.contains('\n'), "bare newline in {line:?}");
        }
        assert!(sdp.ends_with("\r\n"));
    }
}
