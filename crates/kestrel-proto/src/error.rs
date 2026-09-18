//! Error types for parsing and serialising messages.

/// Why a line could not be parsed into a message.
///
/// Parsing is otherwise permissive by design — a peer's malformed tag or stray
/// whitespace should not cost us the whole message — so this list is short.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The line was empty, or held only whitespace and line terminators.
    #[error("empty line")]
    Empty,
    /// The line had tags and/or a source but no command after them.
    #[error("line has no command")]
    MissingCommand,
}

/// Why a message could not be written to the wire.
///
/// Each of these indicates a bug in the code constructing the message rather
/// than bad input, because each would produce a line that parses back into
/// something different from what was built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SerializeError {
    /// The command was empty.
    #[error("command is empty")]
    EmptyCommand,
    /// A parameter other than the last was empty, contained a space, or began
    /// with a colon — things only the trailing parameter may do.
    #[error("parameter {index} can only be represented as the trailing parameter")]
    ParamMustBeLast {
        /// Index of the offending parameter.
        index: usize,
    },
    /// A parameter contained CR, LF or NUL, which cannot be represented.
    #[error("parameter {index} contains CR, LF or NUL")]
    ParamHasControlByte {
        /// Index of the offending parameter.
        index: usize,
    },
}
