//! Wire-format length budgets.
//!
//! Two budgets apply to every IRC line and they are independent of one another.
//! The historical 512-byte limit covers only the message portion; IRCv3
//! `message-tags` adds a separate allowance for the tag section, so server-added
//! tags can never squeeze out a client's own.

/// Maximum message portion in bytes, including the terminating CRLF.
///
/// From RFC 1459: `:prefix COMMAND params\r\n`. Tags are *not* counted here.
pub const MAX_LINE: usize = 512;

/// Maximum message portion excluding the terminating CRLF.
pub const MAX_DATA: usize = MAX_LINE - 2;

/// Maximum size of the whole tag section, counting the leading `@` and the
/// space that separates it from the message portion.
pub const MAX_TAGS_TOTAL: usize = 8191;

/// Maximum tag data a client may send, excluding the `@` and the trailing space.
///
/// Servers get an identical, separate allowance for the tags they add, which is
/// why [`MAX_TAGS_TOTAL`] is roughly twice this value.
pub const MAX_TAG_DATA: usize = 4094;

/// Traditional maximum number of parameters in a single message.
pub const MAX_PARAMS: usize = 15;

/// Line budget Kestrel's own server grants to `CALL SIGNAL`, which carries
/// sealed session descriptions and is exempt from the historical limit.
pub const MAX_SIGNAL_LINE: usize = 8192;
