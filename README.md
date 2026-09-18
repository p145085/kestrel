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

Early development. Text chat and calls both work; the client is not yet
something you would daily-drive.

- [x] `kestrel-proto` — sans-io IRC codec: messages, IRCv3 tags, sources, casemapping, numerics
- [x] `kestrel-proto` shares one codec between client and server, so the wire format cannot drift
- [x] `kestreld-core` — registration, channels, modes, bans, messaging, queries,
      account registration, and the IRCv3 capabilities a modern client expects
- [x] `kestreld-services` — accounts, Argon2 password storage, SASL PLAIN and EXTERNAL
- [x] `kestreld` — the server binary, over plain TCP or TLS, with accounts
      persisted across restarts
- [x] `kestrel-session` — the client's sans-io session: capability
      negotiation, SASL, and tracked channel and member state
- [x] `kestrel-net` — the client transport, plaintext and TLS
- [x] `kestrel-cli` — a terminal client. **Works today**
- [~] `kestrel-ui` — the GTK client. **Connects, joins and chats**, with a
      connection dialog, menu bar, buffer list, member list and topic. The
      Call menu is present but disabled: calls are not wired into it yet
- [x] `xtask bundle` — a folder that runs without GStreamer or GTK installed
- [x] `kestrel-call` — the call state machine: consent, key exchange, mesh
- [x] `kestrel-crypto` — identity, sealed payloads, short authentication strings
- [x] `kestrel-rtc-proto` — compact session descriptions and signalling frames
- [x] `kestrel-media` — GStreamer engine, with camera selection on Windows
- [x] Calls end to end — **two clients call each other through `kestreld`,
      with a real camera or with test patterns**
- [ ] TURN credentials, SFU handoff, and identity that survives a restart

## Building the media engine and the interface

Only needed for `kestrel-media` and `kestrel-ui`; the terminal client and the
server build without either.

```powershell
winget install gstreamerproject.gstreamer
. .\scripts\dev-env.ps1     # sets PKG_CONFIG_PATH and PATH for this shell
cargo test -p kestrel-media -p kestrel-ui
```

The winget package bundles GStreamer, the WebRTC plugins, GTK4 and
`gtk4paintablesink`, so no separate GTK build is needed. On Debian or Ubuntu
the equivalent packages are listed in `.github/workflows/ci.yml`.

## A runnable build

```powershell
. .\scripts\dev-env.ps1
cargo run -p xtask -- bundle
```

That produces `dist\kestrel\`: the server, the terminal client and the
graphical client, together with the libraries they need. It runs on a machine
with neither GStreamer nor GTK installed, and nothing has to be on `PATH`.

- `kestreld.cmd` starts the server on `127.0.0.1:6667`
- `kestrel.cmd` starts a client — run it as many times as you want clients,
  each with its own window, connection and nickname
- `kestrel-ui.exe` can be double-clicked; with no arguments it asks where to
  connect, and **Server → New Connection** opens another one in the same
  process

Only the libraries actually reachable from the programs are copied, which is
read from their import tables rather than listed by hand, and only the twenty
GStreamer plugins a call needs rather than the two hundred that ship. The
bundle then runs its own client with `--check-media` and a bare environment,
so a missing plugin fails the build instead of turning up later as a call that
will not start.

## Trying it from source

```powershell
. .\scripts\dev-env.ps1     # required to RUN the interface, not just to build it
cargo run -p kestreld -- kestreld.toml
cargo run -p kestrel-ui -- 127.0.0.1:6667 --nick you -j '#test'
```

`cargo run` rebuilds first, and Windows will not replace an executable that is
running, so to start a second client build once and launch the binary twice
rather than running `cargo run` again.

GTK's DLLs live in the GStreamer prefix, which nothing puts on `PATH`. Without
that first line the interface dies at startup with `STATUS_DLL_NOT_FOUND`
(`0xc0000135`) before any of its own code runs, so it cannot explain itself.
Packaging will ship the libraries beside the executable; until then, the shell
has to be set up.

For a call, run the terminal client twice and use `/call <nick>`, then
`/answer`. Both ends print a four-word phrase; say it aloud to check nobody
is in the middle, and `/verify` once it matches. `--test-media` uses test
patterns instead of your camera, and `--list-cameras` shows what is
available -- worth checking, since Windows may rank a paired phone ahead of
anything plugged in.

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

## Running the terminal client

```sh
cargo run -p kestrel-cli -- 127.0.0.1:6667 --nick yourname --join '#test'
```

`--tls` connects over TLS, `--sasl <account>` authenticates, and `kestrel
--help` lists the rest. Once connected, `/join`, `/msg`, `/me`, `/topic`,
`/names`, `/whois` and `/raw` work as you would expect; anything else you type
goes to the channel you are looking at.

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
