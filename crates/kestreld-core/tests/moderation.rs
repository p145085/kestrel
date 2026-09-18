//! Channel modes, bans, kicks and invitations.

mod common;

use common::{
    assert_not_received, assert_received, assert_silent, feed, numerics_to, register, test_server,
};
use kestrel_proto::numeric;

/// Register alice (who creates and therefore ops `#chan`) and bob.
fn chan_with_op_and_member() -> (
    kestreld_core::Server,
    kestreld_core::ClientId,
    kestreld_core::ClientId,
) {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");
    (server, alice, bob)
}

// --- querying --------------------------------------------------------------

#[test]
fn querying_a_channels_modes_reports_the_defaults() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    let actions = feed(&mut server, alice, "MODE #chan");

    assert_received(&actions, alice, "324 alice #chan +nt");
    assert!(numerics_to(&actions, alice).contains(&numeric::RPL_CREATIONTIME));
}

#[test]
fn other_peoples_user_modes_are_not_disclosed() {
    let (mut server, alice, _bob) = chan_with_op_and_member();

    let own = feed(&mut server, alice, "MODE alice");
    assert_received(&own, alice, "MODE alice +");

    let other = feed(&mut server, alice, "MODE bob");
    assert_eq!(numerics_to(&other, alice), vec![numeric::ERR_NOSUCHNICK]);
}

// --- privileges ------------------------------------------------------------

#[test]
fn ordinary_members_cannot_change_modes() {
    let (mut server, _alice, bob) = chan_with_op_and_member();
    let actions = feed(&mut server, bob, "MODE #chan +m");

    assert_eq!(
        numerics_to(&actions, bob),
        vec![numeric::ERR_CHANOPRIVSNEEDED]
    );
    assert!(!server.channel(b"#chan").unwrap().modes().moderated);
}

#[test]
fn non_members_cannot_change_modes() {
    let (mut server, _alice, _bob) = chan_with_op_and_member();
    let mallory = register(&mut server, "mallory");

    let actions = feed(&mut server, mallory, "MODE #chan +m");
    assert_eq!(
        numerics_to(&actions, mallory),
        vec![numeric::ERR_NOTONCHANNEL]
    );
}

#[test]
fn an_operator_can_set_flags_and_everyone_is_told() {
    let (mut server, alice, bob) = chan_with_op_and_member();
    let actions = feed(&mut server, alice, "MODE #chan +ms");

    assert_received(&actions, alice, ":alice!~alice@example.host MODE #chan +ms");
    assert_received(&actions, bob, "MODE #chan +ms");

    let modes = server.channel(b"#chan").unwrap().modes().clone();
    assert!(modes.moderated);
    assert!(modes.secret);
}

#[test]
fn a_mode_already_in_effect_is_not_re_announced() {
    // Announcing a no-op change would make clients redraw state that did not
    // move, and makes mode spam a cheap way to flood a channel.
    let (mut server, alice, _bob) = chan_with_op_and_member();

    let actions = feed(&mut server, alice, "MODE #chan +n");
    assert_silent(&actions, alice);
}

#[test]
fn unknown_mode_letters_are_reported_but_the_rest_still_apply() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    let actions = feed(&mut server, alice, "MODE #chan +mZ");

    assert!(numerics_to(&actions, alice).contains(&numeric::ERR_UNKNOWNMODE));
    assert_received(&actions, alice, "MODE #chan +m");
    assert!(server.channel(b"#chan").unwrap().modes().moderated);
}

// --- prefixes --------------------------------------------------------------

#[test]
fn op_and_voice_can_be_granted_and_revoked() {
    let (mut server, alice, bob) = chan_with_op_and_member();

    feed(&mut server, alice, "MODE #chan +v bob");
    assert!(server.channel(b"#chan").unwrap().status(bob).unwrap().voice);

    let actions = feed(&mut server, alice, "MODE #chan +o-v bob bob");
    assert_received(&actions, bob, "MODE #chan +o-v bob bob");

    let status = server.channel(b"#chan").unwrap().status(bob).unwrap();
    assert!(status.operator);
    assert!(!status.voice);
}

#[test]
fn granting_a_prefix_to_a_non_member_is_refused() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    let carol = register(&mut server, "carol");
    let _ = carol;

    let actions = feed(&mut server, alice, "MODE #chan +o carol");
    assert_eq!(
        numerics_to(&actions, alice),
        vec![numeric::ERR_USERNOTINCHANNEL]
    );
}

#[test]
fn a_voiced_member_may_speak_in_a_moderated_channel() {
    let (mut server, alice, bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +m");

    let silenced = feed(&mut server, bob, "PRIVMSG #chan :hello");
    assert_eq!(
        numerics_to(&silenced, bob),
        vec![numeric::ERR_CANNOTSENDTOCHAN]
    );

    feed(&mut server, alice, "MODE #chan +v bob");
    let allowed = feed(&mut server, bob, "PRIVMSG #chan :hello");
    assert_received(&allowed, alice, "PRIVMSG #chan :hello");
}

// --- keys and limits -------------------------------------------------------

#[test]
fn a_key_is_required_to_join_once_set() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +k secret");

    let carol = register(&mut server, "carol");
    let wrong = feed(&mut server, carol, "JOIN #chan wrong");
    assert_eq!(numerics_to(&wrong, carol), vec![numeric::ERR_BADCHANNELKEY]);

    let right = feed(&mut server, carol, "JOIN #chan secret");
    assert_received(&right, carol, "JOIN #chan");
}

#[test]
fn clearing_a_key_takes_its_parameter_but_reopens_the_channel() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +k secret");
    feed(&mut server, alice, "MODE #chan -k secret");

    assert!(server.channel(b"#chan").unwrap().modes().key.is_none());
    let carol = register(&mut server, "carol");
    let actions = feed(&mut server, carol, "JOIN #chan");
    assert_received(&actions, carol, "JOIN #chan");
}

#[test]
fn a_limit_closes_the_channel_when_reached() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +l 2");

    let carol = register(&mut server, "carol");
    let actions = feed(&mut server, carol, "JOIN #chan");
    assert_eq!(
        numerics_to(&actions, carol),
        vec![numeric::ERR_CHANNELISFULL]
    );
}

#[test]
fn a_nonsense_limit_is_ignored_rather_than_wedging_the_channel() {
    let (mut server, alice, _bob) = chan_with_op_and_member();

    for bad in ["+l abc", "+l 0", "+l -5"] {
        feed(&mut server, alice, &format!("MODE #chan {bad}"));
        assert!(
            server.channel(b"#chan").unwrap().modes().limit.is_none(),
            "{bad} should not set a limit"
        );
    }
}

// --- bans ------------------------------------------------------------------

#[test]
fn a_ban_keeps_a_matching_user_out() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +b *!*@example.host");

    let carol = register(&mut server, "carol");
    let actions = feed(&mut server, carol, "JOIN #chan");
    assert_eq!(
        numerics_to(&actions, carol),
        vec![numeric::ERR_BANNEDFROMCHAN]
    );
}

#[test]
fn a_bare_nick_ban_is_expanded_to_a_full_mask() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +b carol");

    let bans = server.channel(b"#chan").unwrap().bans().to_vec();
    assert_eq!(bans.len(), 1);
    assert_eq!(bans[0].mask, b"carol!*@*");

    let carol = register(&mut server, "carol");
    let actions = feed(&mut server, carol, "JOIN #chan");
    assert_eq!(
        numerics_to(&actions, carol),
        vec![numeric::ERR_BANNEDFROMCHAN]
    );
}

#[test]
fn a_ban_silences_a_member_who_is_already_in_the_channel() {
    let (mut server, alice, bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +b bob");

    let actions = feed(&mut server, bob, "PRIVMSG #chan :hello");
    assert_eq!(
        numerics_to(&actions, bob),
        vec![numeric::ERR_CANNOTSENDTOCHAN]
    );
    assert_not_received(&actions, alice, "PRIVMSG");
}

#[test]
fn voicing_a_banned_member_lets_them_speak_again() {
    let (mut server, alice, bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +b bob");
    feed(&mut server, alice, "MODE #chan +v bob");

    let actions = feed(&mut server, bob, "PRIVMSG #chan :hello");
    assert_received(&actions, alice, "PRIVMSG #chan :hello");
}

#[test]
fn an_invitation_overrides_a_ban() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +b carol");
    let carol = register(&mut server, "carol");

    feed(&mut server, alice, "INVITE carol #chan");
    let actions = feed(&mut server, carol, "JOIN #chan");
    assert_received(&actions, carol, "JOIN #chan");
}

#[test]
fn the_ban_list_can_be_read_by_any_member() {
    let (mut server, alice, bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +b *!*@spam.example");

    let actions = feed(&mut server, bob, "MODE #chan b");
    assert_received(&actions, bob, "367 bob #chan *!*@spam.example");
    assert!(numerics_to(&actions, bob).contains(&numeric::RPL_ENDOFBANLIST));
}

#[test]
fn removing_a_ban_lets_the_user_back_in() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +b carol");
    feed(&mut server, alice, "MODE #chan -b carol");

    assert!(server.channel(b"#chan").unwrap().bans().is_empty());
    let carol = register(&mut server, "carol");
    let actions = feed(&mut server, carol, "JOIN #chan");
    assert_received(&actions, carol, "JOIN #chan");
}

// --- kick ------------------------------------------------------------------

#[test]
fn an_operator_can_kick_and_the_target_is_told_why() {
    let (mut server, alice, bob) = chan_with_op_and_member();
    let actions = feed(&mut server, alice, "KICK #chan bob :behave");

    // The kicked client must receive the KICK, or it never learns it left.
    assert_received(
        &actions,
        bob,
        ":alice!~alice@example.host KICK #chan bob :behave",
    );
    assert_received(&actions, alice, "KICK #chan bob :behave");
    assert!(!server.channel(b"#chan").unwrap().contains(bob));
    assert!(server.client(bob).unwrap().channels().is_empty());
}

#[test]
fn kicking_defaults_the_reason_to_the_targets_nick() {
    let (mut server, alice, bob) = chan_with_op_and_member();
    let actions = feed(&mut server, alice, "KICK #chan bob");
    assert_received(&actions, bob, "KICK #chan bob :bob");
}

#[test]
fn ordinary_members_cannot_kick() {
    let (mut server, alice, bob) = chan_with_op_and_member();
    let actions = feed(&mut server, bob, "KICK #chan alice");

    assert_eq!(
        numerics_to(&actions, bob),
        vec![numeric::ERR_CHANOPRIVSNEEDED]
    );
    assert!(server.channel(b"#chan").unwrap().contains(alice));
}

#[test]
fn kicking_someone_who_is_not_in_the_channel_is_refused() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    register(&mut server, "carol");

    let actions = feed(&mut server, alice, "KICK #chan carol");
    assert_eq!(
        numerics_to(&actions, alice),
        vec![numeric::ERR_USERNOTINCHANNEL]
    );
}

// --- invite ----------------------------------------------------------------

#[test]
fn invite_only_channels_require_an_invitation() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +i");
    let carol = register(&mut server, "carol");

    let refused = feed(&mut server, carol, "JOIN #chan");
    assert_eq!(
        numerics_to(&refused, carol),
        vec![numeric::ERR_INVITEONLYCHAN]
    );

    let invite = feed(&mut server, alice, "INVITE carol #chan");
    assert_received(
        &invite,
        carol,
        ":alice!~alice@example.host INVITE carol #chan",
    );
    assert!(numerics_to(&invite, alice).contains(&numeric::RPL_INVITING));

    let accepted = feed(&mut server, carol, "JOIN #chan");
    assert_received(&accepted, carol, "JOIN #chan");
}

#[test]
fn an_invitation_is_consumed_by_joining() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +i");
    let carol = register(&mut server, "carol");

    feed(&mut server, alice, "INVITE carol #chan");
    feed(&mut server, carol, "JOIN #chan");
    feed(&mut server, carol, "PART #chan");

    // The invitation was spent on the first join; a second needs a new one.
    let actions = feed(&mut server, carol, "JOIN #chan");
    assert_eq!(
        numerics_to(&actions, carol),
        vec![numeric::ERR_INVITEONLYCHAN]
    );
}

#[test]
fn ordinary_members_cannot_invite_past_plus_i() {
    let (mut server, alice, bob) = chan_with_op_and_member();
    feed(&mut server, alice, "MODE #chan +i");
    register(&mut server, "carol");

    let actions = feed(&mut server, bob, "INVITE carol #chan");
    assert_eq!(
        numerics_to(&actions, bob),
        vec![numeric::ERR_CHANOPRIVSNEEDED]
    );
}

#[test]
fn inviting_an_existing_member_is_refused() {
    let (mut server, alice, _bob) = chan_with_op_and_member();
    let actions = feed(&mut server, alice, "INVITE bob #chan");
    assert_eq!(
        numerics_to(&actions, alice),
        vec![numeric::ERR_USERONCHANNEL]
    );
}
