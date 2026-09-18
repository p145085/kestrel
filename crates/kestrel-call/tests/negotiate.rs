//! Two call state machines driving each other end to end.
//!
//! No sockets and no GStreamer: signalling payloads are handed across
//! directly, and the media engine is replaced by a few lines that produce the
//! SDP a real one would. What is exercised is the whole of the logic — key
//! agreement, consent, verification, teardown.

use kestrel_call::{Action, Call, CallError, Event, MediaEvent, Privacy};
use kestrel_crypto::{Identity, KnownPeers, Trust};
use kestrel_rtc_proto::{Profile, SessionDescription, Setup, sdp};

const CALL_ID: &[u8] = b"c1";

fn call_for(account: &str) -> Call {
    Call::new(CALL_ID, account, Identity::generate(), KnownPeers::new())
}

/// The SDP a media engine would produce.
fn offer_sdp(setup: Setup) -> String {
    sdp::to_sdp(&SessionDescription::new(
        [0x11; 32],
        "ufrag",
        "a-long-ice-password",
        setup,
        Profile::OpusVp8,
        1,
        Some(2),
    ))
}

/// Every payload an outcome wants sent.
fn signals(outcome: &kestrel_call::Outcome) -> Vec<String> {
    outcome
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::Signal { payload, .. } => Some(payload.clone()),
            _ => None,
        })
        .collect()
}

fn has_action(outcome: &kestrel_call::Outcome, matcher: impl Fn(&Action) -> bool) -> bool {
    outcome.actions.iter().any(matcher)
}

/// Ring bob from alice, and have bob answer.
fn ringing_and_answered() -> (Call, Call) {
    let mut alice = call_for("alice");
    let mut bob = call_for("bob");

    let invite = alice.invite("bob").expect("invite should build");
    let ringing = bob
        .on_signal("alice", Some("alice"), &signals(&invite)[0])
        .expect("invite should be readable");
    assert!(matches!(ringing.events[0], Event::Ringing { .. }));

    let accept = bob.accept("alice").expect("bob should be able to answer");
    alice
        .on_signal("bob", Some("bob"), &signals(&accept)[0])
        .expect("acceptance should be readable");

    (alice, bob)
}

// --- consent ---------------------------------------------------------------

#[test]
fn an_invitation_gathers_nothing_until_it_is_answered() {
    // Ringing somebody must not tell you where they are. If a candidate were
    // gathered here, simply calling a person would disclose their address
    // whether or not they picked up.
    let mut alice = call_for("alice");
    let mut bob = call_for("bob");

    let invite = alice.invite("bob").unwrap();
    assert!(
        !has_action(&invite, |a| matches!(a, Action::OpenPeer { .. })),
        "the caller must not open a connection before the callee answers"
    );

    let ringing = bob
        .on_signal("alice", Some("alice"), &signals(&invite)[0])
        .unwrap();
    assert!(
        !has_action(&ringing, |a| matches!(a, Action::OpenPeer { .. })),
        "ringing must not open a connection"
    );
    assert!(signals(&ringing).is_empty(), "ringing must send nothing back");
}

#[test]
fn answering_is_what_starts_gathering() {
    let mut alice = call_for("alice");
    let mut bob = call_for("bob");
    let invite = alice.invite("bob").unwrap();
    bob.on_signal("alice", Some("alice"), &signals(&invite)[0])
        .unwrap();

    let accept = bob.accept("alice").unwrap();
    assert!(has_action(&accept, |a| matches!(a, Action::OpenPeer { .. })));
}

#[test]
fn only_the_caller_offers() {
    // Both ends offering at once is the classic glare failure.
    let (_alice, bob) = ringing_and_answered();
    let _ = bob;

    let mut alice = call_for("alice");
    let mut bob = call_for("bob");
    let invite = alice.invite("bob").unwrap();
    bob.on_signal("alice", Some("alice"), &signals(&invite)[0])
        .unwrap();

    let accept = bob.accept("alice").unwrap();
    assert!(
        !has_action(&accept, |a| matches!(a, Action::CreateOffer { .. })),
        "the callee must not offer"
    );

    let on_accept = alice
        .on_signal("bob", Some("bob"), &signals(&accept)[0])
        .unwrap();
    assert!(has_action(&on_accept, |a| matches!(
        a,
        Action::CreateOffer { .. }
    )));
}

// --- the full exchange -----------------------------------------------------

#[test]
fn a_description_travels_sealed_and_arrives_expanded() {
    let (mut alice, mut bob) = ringing_and_answered();

    let offer = alice
        .on_media(MediaEvent::LocalDescription {
            peer: "bob".to_owned(),
            kind: "offer".to_owned(),
            sdp: offer_sdp(Setup::ActPass),
        })
        .unwrap();

    let payload = &signals(&offer)[0];
    assert!(
        !payload.contains("v=0") && !payload.contains("ice-ufrag"),
        "the description must not be readable by the server relaying it"
    );
    // The compact form is what makes an offer fit a single protocol line.
    assert!(payload.len() < 512, "offer was {} bytes", payload.len());

    let received = bob.on_signal("alice", Some("alice"), payload).unwrap();
    let Some(Action::SetRemoteDescription { kind, sdp: text, .. }) = received
        .actions
        .iter()
        .find(|a| matches!(a, Action::SetRemoteDescription { .. }))
    else {
        panic!("bob should have been given a description, got {received:?}");
    };
    assert_eq!(kind, "offer");
    assert!(text.contains("a=ice-ufrag:ufrag"), "got {text}");
    assert!(
        has_action(&received, |a| matches!(a, Action::CreateAnswer { .. })),
        "the callee should answer once the offer is applied"
    );
}

#[test]
fn candidates_travel_sealed_and_arrive_as_candidates() {
    let (mut alice, mut bob) = ringing_and_answered();

    let outcome = alice
        .on_media(MediaEvent::LocalCandidate {
            peer: "bob".to_owned(),
            mline_index: 0,
            candidate: "candidate:1 1 udp 2130706431 192.168.1.5 9000 typ host".to_owned(),
        })
        .unwrap();

    let payload = &signals(&outcome)[0];
    assert!(
        !payload.contains("192.168"),
        "an address must not be readable in transit"
    );

    let received = bob.on_signal("alice", Some("alice"), payload).unwrap();
    let Some(Action::AddCandidate { candidate, .. }) = received
        .actions
        .iter()
        .find(|a| matches!(a, Action::AddCandidate { .. }))
    else {
        panic!("bob should have been given a candidate, got {received:?}");
    };
    assert!(candidate.contains("192.168.1.5"), "got {candidate}");
}

#[test]
fn relay_only_never_sends_an_address() {
    // A privacy promise that leaks under any condition is not a promise.
    let mut alice = call_for("alice").with_privacy(Privacy::RelayOnly);
    let mut bob = call_for("bob");
    let invite = alice.invite("bob").unwrap();
    bob.on_signal("alice", Some("alice"), &signals(&invite)[0])
        .unwrap();
    let accept = bob.accept("alice").unwrap();
    alice
        .on_signal("bob", Some("bob"), &signals(&accept)[0])
        .unwrap();

    for line in [
        "candidate:1 1 udp 2130706431 192.168.1.5 9000 typ host",
        "candidate:2 1 udp 1694498815 203.0.113.9 9001 typ srflx",
    ] {
        let outcome = alice
            .on_media(MediaEvent::LocalCandidate {
                peer: "bob".to_owned(),
                mline_index: 0,
                candidate: line.to_owned(),
            })
            .unwrap();
        assert!(
            signals(&outcome).is_empty(),
            "{line} discloses an address and must not be sent"
        );
    }

    let relayed = alice
        .on_media(MediaEvent::LocalCandidate {
            peer: "bob".to_owned(),
            mline_index: 0,
            candidate: "candidate:3 1 udp 16777215 198.51.100.1 3478 typ relay".to_owned(),
        })
        .unwrap();
    assert_eq!(signals(&relayed).len(), 1, "a relayed candidate is fine");
}

// --- identity --------------------------------------------------------------

#[test]
fn both_ends_see_the_same_phrase() {
    // If they differed, comparing them aloud would prove nothing.
    let mut alice = call_for("alice");
    let mut bob = call_for("bob");
    let invite = alice.invite("bob").unwrap();
    bob.on_signal("alice", Some("alice"), &signals(&invite)[0])
        .unwrap();

    let accept = bob.accept("alice").unwrap();
    let bob_phrase = phrase(&accept);

    let on_accept = alice
        .on_signal("bob", Some("bob"), &signals(&accept)[0])
        .unwrap();
    let alice_phrase = phrase(&on_accept);

    assert_eq!(alice_phrase, bob_phrase);
    assert_eq!(
        alice_phrase.split_whitespace().count(),
        4,
        "got {alice_phrase}"
    );
}

fn phrase(outcome: &kestrel_call::Outcome) -> String {
    outcome
        .events
        .iter()
        .find_map(|e| match e {
            Event::Verify { sas, .. } => Some(sas.phrase()),
            _ => None,
        })
        .expect("a phrase should have been produced")
}

#[test]
fn a_first_call_reports_a_new_key_and_a_second_reports_it_known() {
    let mut alice = call_for("alice");
    let bob_identity = Identity::generate();

    let first = ring_from(&mut alice, &bob_identity, "bob");
    assert!(matches!(trust(&first), Trust::New));

    let second = ring_from(&mut alice, &bob_identity, "bob");
    assert!(
        matches!(trust(&second), Trust::Known),
        "the same key should be recognised"
    );
}

#[test]
fn a_changed_key_is_reported_rather_than_accepted() {
    // A new device and an impersonator look identical from here, so this is
    // the user's decision, not ours.
    let mut alice = call_for("alice");
    ring_from(&mut alice, &Identity::generate(), "bob");

    let impostor = ring_from(&mut alice, &Identity::generate(), "bob");
    assert!(
        matches!(trust(&impostor), Trust::Changed { .. }),
        "a different key for the same account must be flagged"
    );
}

/// Have `peer` call `us` with the given identity, and answer.
fn ring_from(us: &mut Call, identity: &Identity, peer: &str) -> kestrel_call::Outcome {
    let mut them = Call::new(
        CALL_ID,
        peer,
        Identity::from_secret(identity.to_secret()),
        KnownPeers::new(),
    );
    let invite = them.invite("alice").unwrap();
    us.on_signal(peer, Some(peer), &signals(&invite)[0]).unwrap();
    us.accept(peer).unwrap()
}

fn trust(outcome: &kestrel_call::Outcome) -> Trust {
    outcome
        .events
        .iter()
        .find_map(|e| match e {
            Event::Verify { trust, .. } => Some(*trust),
            _ => None,
        })
        .expect("a verification should have been produced")
}

#[test]
fn a_forged_invitation_is_refused() {
    let mut alice = call_for("alice");
    let mut bob = call_for("bob");
    let invite = alice.invite("bob").unwrap();

    // Flip a character in the middle of the payload, not at the end: the
    // final base64 character carries padding bits, so several values there
    // decode to the same bytes and the tampering would be undone by the
    // decoder rather than caught by the signature.
    let original = signals(&invite)[0].clone();
    let middle = original.len() / 2;
    let mut tampered = original.clone();
    let replacement = if original.as_bytes()[middle] == b'A' {
        'B'
    } else {
        'A'
    };
    tampered.replace_range(middle..=middle, &replacement.to_string());
    assert_ne!(tampered, original, "the payload was not actually changed");

    assert!(bob.on_signal("alice", Some("alice"), &tampered).is_err());
}

#[test]
fn a_signature_from_another_call_does_not_transfer() {
    let mut alice = call_for("alice");
    let invite = alice.invite("bob").unwrap();

    // The same invitation offered up as belonging to a different call.
    let mut other_call = Call::new(b"c2", "bob", Identity::generate(), KnownPeers::new());
    assert_eq!(
        other_call.on_signal("alice", Some("alice"), &signals(&invite)[0]),
        Err(CallError::BadSignature)
    );
}

// --- teardown --------------------------------------------------------------

#[test]
fn hanging_up_tells_the_peer_and_closes_the_connection() {
    let (mut alice, mut bob) = ringing_and_answered();

    let bye = alice.hang_up("bob", "done").unwrap();
    assert!(has_action(&bye, |a| matches!(a, Action::ClosePeer { .. })));
    assert!(!alice.is_active("bob"));

    let received = bob.on_signal("alice", Some("alice"), &signals(&bye)[0]).unwrap();
    assert!(has_action(&received, |a| matches!(a, Action::ClosePeer { .. })));
    assert!(matches!(received.events[0], Event::PeerGone { .. }));
    assert!(!bob.is_active("alice"));
}

#[test]
fn hanging_up_on_an_unanswered_call_needs_no_key() {
    // There is no shared secret yet, so there is nothing to tell them with;
    // dropping them locally has to be enough rather than an error.
    let mut alice = call_for("alice");
    alice.invite("bob").unwrap();

    let bye = alice.hang_up("bob", "changed my mind").unwrap();
    assert!(has_action(&bye, |a| matches!(a, Action::ClosePeer { .. })));
    assert!(!alice.is_active("bob"));
}

#[test]
fn a_peer_we_do_not_know_is_refused() {
    let mut alice = call_for("alice");
    assert_eq!(alice.accept("nobody"), Err(CallError::NoSuchPeer));
    assert_eq!(alice.hang_up("nobody", "why"), Err(CallError::NoSuchPeer));
}

#[test]
fn a_sealed_frame_before_the_exchange_completes_is_refused() {
    let (mut alice, _bob) = ringing_and_answered();
    let mut stranger = call_for("stranger");
    stranger.invite("alice").unwrap();

    // Something that looks sealed but was never agreed with us.
    assert!(alice.on_signal("bob", Some("bob"), "bm90LWEtcmVhbC1mcmFtZQ").is_err());
}

#[test]
fn a_media_failure_is_reported_without_ending_the_call() {
    let (mut alice, _bob) = ringing_and_answered();
    let outcome = alice
        .on_media(MediaEvent::Failed {
            peer: "bob".to_owned(),
            reason: "ice failed".to_owned(),
        })
        .unwrap();

    assert!(matches!(outcome.events[0], Event::PeerFailed { .. }));
    assert!(
        alice.is_active("bob"),
        "one peer failing should not tear down the whole call"
    );
}
