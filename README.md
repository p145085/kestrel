# Kestrel

An IRC client with native audio and video conferencing — and the server to go with it.

IRC is still the best open text-chat protocol: federated, scriptable, no vendor, no
account required. It has never had an answer for voice and video, so communities that
want a call leave for Discord or Matrix and generally never come back.

Kestrel is three things:

| | What it is |
|---|---|
| **kestrel** | A GTK4 IRC client. A first-class text client first — good enough to replace HexChat or WeeChat for daily use — that can also start an encrypted call in any channel or DM. |
| **kestreld** | An IRC server in Rust with calls as a native protocol feature rather than a bolt-on. |
| **the spec** | An open specification for RTC over IRC, plus reference patches so existing networks can adopt calls if they want them. |

## Design commitments

**Calls are peer-to-peer and end-to-end encrypted.** Media flows directly between
participants over DTLS-SRTP. No server sits in the middle of a mesh call, including ours.

**Your IP address is not disclosed without your consent.** ICE candidates are not even
gathered until you accept a call, so an unanswered call leaks nothing. Relay-only mode
hides your address from other participants entirely, and the client tells you plainly
which mode a call is in. Where a privacy guarantee cannot be met, Kestrel refuses the
call rather than quietly downgrading.

**A hostile server cannot listen in.** Signalling payloads are sealed to a specific peer,
so the server relays ciphertext it cannot read. Identities are pinned on first use, and
a short authentication string lets two people confirm out loud that nobody is in the middle.

**The client never lies about encryption.** When an SFU is used for a large call, media is
encrypted in transit but the server can decrypt it. The UI says so.

## Status

Early development. Nothing is usable yet.

- [x] `kestrel-proto` — sans-io IRC codec: messages, IRCv3 tags, sources, casemapping, numerics
- [x] `kestrel-proto` shares one codec between client and server, so the wire format cannot drift
- [x] `kestreld-core` — registration, channels, modes, bans, messaging, queries,
      account registration, and the IRCv3 capabilities a modern client expects
- [x] `kestreld-services` — accounts, Argon2 password storage, SASL PLAIN and EXTERNAL
- [~] `kestreld` — the server binary. Runs over plain TCP; **no TLS yet**, and
      nothing is persisted across restarts
- [ ] `kestrel` — the client
- [ ] Calls

**The server works.** You can point HexChat, WeeChat or irssi at it today and
chat: register, join channels, set modes and topics, kick, ban, and authenticate
with SASL. What is missing is TLS, persistence, and every part of the calls
feature — so this is worth running locally, and not worth running publicly.

## Running the server

```sh
cargo run -p kestreld -- kestreld.example.toml
```

Then connect a client to `127.0.0.1:6667`. `kestreld --print-config` prints a
default configuration file to start from.

## Building

Requires a recent stable Rust toolchain.

```sh
cargo test --workspace
cargo clippy --all-targets
cargo fmt --all
```

The protocol crates are sans-io and sans-clock — messages and the current time
come in as arguments — so the whole of the server's behaviour is tested
in-process, without sockets or a scheduler. `crates/kestreld/tests` adds
end-to-end tests over real TCP for the parts that only exist once there is a
socket: line framing across packet boundaries, and abrupt disconnects.

Later phases add GTK4 and GStreamer, which are large native dependencies; build
instructions for each platform will land alongside the crates that need them.

## Contributing

Contributions are welcome. The protocol crates are sans-io by design — no sockets, no
async runtime, no clock — so they can be tested exhaustively without a network or a
display server. Please keep it that way, and please add tests.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
