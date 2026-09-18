# RTC over IRC

**Status:** draft · **Version:** 0 · **Vendor prefix:** `kestrel.chat`

This describes how Kestrel places audio and video calls over IRC. It is written
so that another server or client can implement it, and so that a network can
decide whether it wants to.

## What this is for

IRC has no voice or video. Communities that want a call leave for something
else. This adds calls without changing what IRC is: the server routes
*signalling*, never media. Media flows peer to peer, encrypted end to end,
and the server cannot decrypt it.

## What the server does and does not do

The server:

- keeps a registry of calls and who is in them, so a client joining a channel
  can see a call is happening;
- relays sealed signalling payloads between participants;
- optionally issues short-lived TURN credentials;
- optionally points large calls at a media server.

The server does **not**:

- see or carry media;
- read signalling payloads, which are encrypted to a specific recipient;
- decide who may speak to whom beyond ordinary channel permissions.

## Capability

```
CAP REQ :kestrel.chat/rtc
```

Advertised with parameters a client needs before it offers a call button:

```
kestrel.chat/rtc=ver=0,maxsig=8192,maxmesh=8[,turn=1][,sfu=1]
```

| Parameter | Meaning |
|---|---|
| `ver` | Protocol version. A client that does not understand it must not offer calls. |
| `maxsig` | Largest `CALL SIGNAL` payload in bytes. |
| `maxmesh` | Largest mesh call the server will allow. |
| `turn` | Present when `CALL TURN` will issue credentials. |
| `sfu` | Present when `CALL SFU` will issue a media-server token. |

A server MUST refuse `CALL` commands from a client that has not negotiated the
capability: such a client has no way to be told what happens next.

## Commands

```
CALL START   <target> [media]
CALL INVITE  <call-id> <nick>
CALL ACCEPT  <call-id>
CALL DECLINE <call-id> [reason]
CALL LEAVE   <call-id>
CALL LIST    <target>
CALL SIGNAL  <call-id> <dst> :<payload>
CALL TURN    <call-id>
CALL SFU     <call-id>
```

`<target>` is a channel or a nickname. `<call-id>` is an opaque token the
server assigns; clients MUST NOT parse it. `[media]` is a comma-separated
subset of `audio,video,screen` and defaults to `audio,video`.

### Starting and joining

`CALL START` on a channel creates a call if none exists and adds the caller,
or joins the existing one. The caller must be a channel member, and ordinary
channel permissions apply: a client that may not speak in a channel may not
call in it.

`CALL START` on a nickname creates a call and invites that nickname. It does
not add them — being called is not the same as answering.

The server replies to the caller:

```
:server CALL <call-id> STARTED <target> <media>
```

and tells the channel:

```
:nick!user@host CALL <call-id> JOINED <target>
```

Everyone already in the call learns who joined; everyone in the channel who
negotiated the capability learns a call is in progress.

### Answering

An invited client receives:

```
:nick!user@host CALL <call-id> INVITE <target> <media>
```

and answers with `CALL ACCEPT` or `CALL DECLINE`. Until it accepts, it MUST
NOT gather ICE candidates — see *Privacy* below.

### Leaving

`CALL LEAVE` removes the client. The server tells the remaining participants:

```
:nick!user@host CALL <call-id> LEFT <target> [reason]
```

A call with no participants left ceases to exist. Quitting, being kicked from
the channel, or parting it all count as leaving the call: a client that can no
longer see the channel must not stay in its call.

### Discovery

`CALL LIST <target>` reports what is happening:

```
:server CALL <call-id> INFO <target> <media> <participant-count>
:server CALL <call-id> PARTICIPANT <target> <nick> <account>
:server CALL * END <target> :End of call list
```

`<account>` is `*` for a participant who has not authenticated. Clients SHOULD
show that difference: a nickname is whoever holds it this second, an account is
a person.

## Signalling

```
CALL SIGNAL <call-id> <dst> :<payload>
```

`<dst>` is a participant's nickname, or `*` for everyone else in the call. The
server relays it unchanged:

```
:nick!user@host CALL <call-id> SIGNAL <payload>
```

The payload is opaque to the server: base64url, no padding, sealed to the
recipient. Servers MUST NOT interpret, rewrite or log it.

A server MUST reject a `CALL SIGNAL` whose sender is not in the call, and MUST
NOT deliver one to a nickname that is not.

**Rate limits.** Signalling is exempt from ordinary flood accounting, because
call setup is bursty by nature and a client that gets throttled mid-negotiation
fails in a way the user reads as "the call did not work". A server SHOULD
instead limit signalling separately, generously enough that a mesh join never
hits it.

## Payload format

Payload contents are not the server's business, so this section describes only
what Kestrel's own clients exchange. Another implementation may use its own
format; the server does not care.

Payloads are CBOR, sealed with ChaCha20-Poly1305 under a key derived from an
X25519 exchange, then base64url-encoded.

A **compact session description** carries only what varies between peers — DTLS
fingerprint, ICE credentials, setup role, codec profile identifier, SSRCs —
and each side reconstitutes full SDP from a shared template table. This is
about 200 bytes rather than the 3–5 KB a full SDP offer would be, which is what
keeps a mesh join inside a handful of lines.

ICE uses trickle, always. This is not only an optimisation: it is what allows
candidate disclosure to be withheld until the callee has accepted.

## Privacy

WebRTC discloses participants' IP addresses to each other through ICE
candidates. On a network where cloaks have always hidden them, that is a
change users must be able to see and refuse.

**Candidates are not gathered until the call is accepted.** An invitation
carries no fingerprint and no candidates, so an unanswered call — including a
spammed one — discloses nothing beyond the fact that somebody tried.

**Payloads are sealed to a specific recipient.** The server relays ciphertext.
Observers, including the server, learn who is negotiating with whom, never an
address or a fingerprint.

**Clients SHOULD offer a relay-only mode** that gathers only TURN candidates,
and SHOULD make the current mode visible during a call. Where a privacy
guarantee cannot be met — relay-only requested but no TURN available — a client
SHOULD refuse the call rather than quietly downgrade.

## Identity

A nickname is not an identity. Call identity binds to the services account,
which is why `account-tag` and `sasl` are effectively prerequisites.

Because the server relays signalling, a hostile or compromised server could
substitute DTLS fingerprints and place itself in the middle. Two defences,
both at the client:

- **Fingerprints travel signed and sealed**, never in the clear.
- **A short authentication string** derived from both parties' identity keys,
  shown to both and confirmed out of band. This is the only defence that
  survives a malicious server, and it costs one dialogue.

Clients SHOULD pin an account's identity key on first use and warn loudly when
it changes.

## Media confidentiality

A mesh call is end to end encrypted: DTLS-SRTP between each pair, no
intermediary. An SFU is not — it terminates DTLS and can decrypt. A client
MUST distinguish the two in its interface. Saying "encrypted" about a call a
server can listen to is worse than saying nothing.

## Errors

Errors use `standard-replies`:

```
FAIL CALL NO_SUCH_CALL <call-id> :No such call
FAIL CALL NOT_IN_CALL <call-id> :You are not in that call
FAIL CALL NOT_IN_CHANNEL <target> :You are not on that channel
FAIL CALL CALL_FULL <call-id> :That call is full
FAIL CALL NO_SUCH_TARGET <target> :No such nick or channel
FAIL CALL INVALID_PAYLOAD <call-id> :Malformed signalling payload
FAIL CALL RTC_DISABLED :Calls are not enabled on this server
```

## Adoption

The whole of this is optional. A server that implements none of it is still a
working IRC server, and a client that sees no `kestrel.chat/rtc` capability
simply does not offer calls.

A network wanting calls needs the call registry and `CALL SIGNAL` relay; TURN
and SFU support are separable and can be added later or never. Reference
patches for other ircds are the intended route, and the vendor prefix becomes
`draft/` if the IRCv3 working group takes it up.
