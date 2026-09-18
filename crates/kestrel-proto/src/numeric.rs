//! Numeric reply codes.
//!
//! Only the numerics Kestrel actually sends or handles are listed. The set is
//! deliberately not exhaustive: IRC has accumulated hundreds of numerics,
//! many contradictory between server families, and a list of constants nobody
//! uses is a list nobody keeps correct.

// Registration and server information.

/// First message after successful registration.
pub const RPL_WELCOME: u16 = 1;
/// Names the server and its software version.
pub const RPL_YOURHOST: u16 = 2;
/// When the server was started.
pub const RPL_CREATED: u16 = 3;
/// Server name, version, and available user and channel modes.
pub const RPL_MYINFO: u16 = 4;
/// Advertises server capabilities and limits as `TOKEN=value` pairs.
pub const RPL_ISUPPORT: u16 = 5;

// Network statistics, sent after registration by convention.

/// Count of visible users, invisible users, and servers.
pub const RPL_LUSERCLIENT: u16 = 251;
/// Number of connected operators.
pub const RPL_LUSEROP: u16 = 252;
/// Number of unknown connections.
pub const RPL_LUSERUNKNOWN: u16 = 253;
/// Number of formed channels.
pub const RPL_LUSERCHANNELS: u16 = 254;
/// Clients and servers connected to this server.
pub const RPL_LUSERME: u16 = 255;
/// Current and maximum local user count.
pub const RPL_LOCALUSERS: u16 = 265;
/// Current and maximum global user count.
pub const RPL_GLOBALUSERS: u16 = 266;

// Message of the day.

/// Start of the message of the day.
pub const RPL_MOTDSTART: u16 = 375;
/// One line of the message of the day.
pub const RPL_MOTD: u16 = 372;
/// End of the message of the day.
pub const RPL_ENDOFMOTD: u16 = 376;
/// No message of the day is configured.
pub const ERR_NOMOTD: u16 = 422;

// Away status.

/// Sent when messaging a user who is away.
pub const RPL_AWAY: u16 = 301;
/// Confirms the client is no longer away.
pub const RPL_UNAWAY: u16 = 305;
/// Confirms the client is now away.
pub const RPL_NOWAWAY: u16 = 306;

// Channels.

/// The channel has no topic set.
pub const RPL_NOTOPIC: u16 = 331;
/// The channel topic.
pub const RPL_TOPIC: u16 = 332;
/// Who set the topic, and when.
pub const RPL_TOPICWHOTIME: u16 = 333;
/// A batch of channel members.
pub const RPL_NAMREPLY: u16 = 353;
/// End of a `NAMES` listing.
pub const RPL_ENDOFNAMES: u16 = 366;
/// The channel's current modes.
pub const RPL_CHANNELMODEIS: u16 = 324;
/// When the channel was created.
pub const RPL_CREATIONTIME: u16 = 329;
/// Start of a `LIST` listing.
pub const RPL_LISTSTART: u16 = 321;
/// One channel in a `LIST` listing.
pub const RPL_LIST: u16 = 322;
/// End of a `LIST` listing.
pub const RPL_LISTEND: u16 = 323;
/// One entry of a channel ban list.
pub const RPL_BANLIST: u16 = 367;
/// End of a channel ban list.
pub const RPL_ENDOFBANLIST: u16 = 368;
/// Confirms an invitation was sent.
pub const RPL_INVITING: u16 = 341;

// WHO and WHOIS.

/// One entry of a `WHO` listing.
pub const RPL_WHOREPLY: u16 = 352;
/// End of a `WHO` listing.
pub const RPL_ENDOFWHO: u16 = 315;
/// One entry of a `WHOX` listing.
pub const RPL_WHOSPCRPL: u16 = 354;
/// Identity of the user being queried.
pub const RPL_WHOISUSER: u16 = 311;
/// Which server the user is on.
pub const RPL_WHOISSERVER: u16 = 312;
/// The user is an operator.
pub const RPL_WHOISOPERATOR: u16 = 313;
/// How long the user has been idle.
pub const RPL_WHOISIDLE: u16 = 317;
/// Channels the user is in.
pub const RPL_WHOISCHANNELS: u16 = 319;
/// The user is logged in to this services account.
pub const RPL_WHOISACCOUNT: u16 = 330;
/// The user is connected securely.
pub const RPL_WHOISSECURE: u16 = 671;
/// End of a `WHOIS` reply.
pub const RPL_ENDOFWHOIS: u16 = 318;

// Errors.

/// No such nickname.
pub const ERR_NOSUCHNICK: u16 = 401;
/// No such server.
pub const ERR_NOSUCHSERVER: u16 = 402;
/// No such channel.
pub const ERR_NOSUCHCHANNEL: u16 = 403;
/// Cannot send to this channel.
pub const ERR_CANNOTSENDTOCHAN: u16 = 404;
/// The client has joined too many channels.
pub const ERR_TOOMANYCHANNELS: u16 = 405;
/// No recipient was given.
pub const ERR_NORECIPIENT: u16 = 411;
/// No text to send.
pub const ERR_NOTEXTTOSEND: u16 = 412;
/// Unknown command.
pub const ERR_UNKNOWNCOMMAND: u16 = 421;
/// No nickname was given.
pub const ERR_NONICKNAMEGIVEN: u16 = 431;
/// The nickname contains invalid characters.
pub const ERR_ERRONEUSNICKNAME: u16 = 432;
/// The nickname is already taken.
pub const ERR_NICKNAMEINUSE: u16 = 433;
/// That user is not in the channel.
pub const ERR_USERNOTINCHANNEL: u16 = 441;
/// The client is not in that channel.
pub const ERR_NOTONCHANNEL: u16 = 442;
/// That user is already in the channel.
pub const ERR_USERONCHANNEL: u16 = 443;
/// The client has not completed registration.
pub const ERR_NOTREGISTERED: u16 = 451;
/// The command needs more parameters.
pub const ERR_NEEDMOREPARAMS: u16 = 461;
/// The client is already registered.
pub const ERR_ALREADYREGISTERED: u16 = 462;
/// The supplied password was wrong.
pub const ERR_PASSWDMISMATCH: u16 = 464;
/// The channel is full.
pub const ERR_CHANNELISFULL: u16 = 471;
/// Unknown mode character.
pub const ERR_UNKNOWNMODE: u16 = 472;
/// The channel is invite-only.
pub const ERR_INVITEONLYCHAN: u16 = 473;
/// The client is banned from the channel.
pub const ERR_BANNEDFROMCHAN: u16 = 474;
/// The channel key was wrong.
pub const ERR_BADCHANNELKEY: u16 = 475;
/// Channel operator privileges are required.
pub const ERR_CHANOPRIVSNEEDED: u16 = 482;

// Capability negotiation and SASL.

/// The client is now logged in to a services account.
pub const RPL_LOGGEDIN: u16 = 900;
/// The client is no longer logged in.
pub const RPL_LOGGEDOUT: u16 = 901;
/// The account has been disabled.
pub const ERR_NICKLOCKED: u16 = 902;
/// Authentication succeeded.
pub const RPL_SASLSUCCESS: u16 = 903;
/// Authentication failed.
pub const ERR_SASLFAIL: u16 = 904;
/// The authentication message was too long.
pub const ERR_SASLTOOLONG: u16 = 905;
/// Authentication was aborted by the client.
pub const ERR_SASLABORTED: u16 = 906;
/// The client has already authenticated.
pub const ERR_SASLALREADY: u16 = 907;
/// Lists the mechanisms the server supports.
pub const RPL_SASLMECHS: u16 = 908;

/// Format a numeric as the three ASCII digits that go on the wire.
///
/// Numerics are always three digits, so `1` is written `001`.
#[must_use]
pub fn to_wire(code: u16) -> [u8; 3] {
    let code = code % 1000;
    [
        b'0' + (code / 100) as u8,
        b'0' + (code / 10 % 10) as u8,
        b'0' + (code % 10) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::{RPL_ENDOFNAMES, RPL_WELCOME, to_wire};

    #[test]
    fn single_digit_numerics_are_zero_padded() {
        assert_eq!(&to_wire(RPL_WELCOME), b"001");
        assert_eq!(&to_wire(5), b"005");
    }

    #[test]
    fn three_digit_numerics_are_unchanged() {
        assert_eq!(&to_wire(RPL_ENDOFNAMES), b"366");
        assert_eq!(&to_wire(999), b"999");
    }

    #[test]
    fn wire_form_round_trips_through_the_parser() {
        for code in [1_u16, 5, 42, 366, 433, 903] {
            let line = [&to_wire(code)[..], b" nick :text"].concat();
            let msg = crate::Message::parse(&line).expect("should parse");
            assert_eq!(msg.numeric(), Some(code));
        }
    }
}
