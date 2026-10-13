# Design and limits

This document describes how nrps relays messages, the limits it enforces, what it logs,
and its command-line options. You do not need it to install or use nrps.

## Protocol

The protocol follows NVDA's own local relay (`source/_remoteClient/server.py` in the
NVDA source): TLS on port 6837, one JSON object per line.

A client sends `protocol_version`, then either `join` (with `channel` set to the key
and `connection_type` set to `master` for the controlling computer or `slave` for the
controlled computer) or `generate_key`. Before the join, other messages are ignored;
the authentication timeout still applies.

nrps sends `channel_joined`, `client_joined` and `client_left` as NVDA expects, and a
`ping` every 120 seconds by default (`--ping-interval`).

There is no locked-channel or password extension: the key is the only secret.

## Routing

Once joined, messages are routed by role:

- from a controlling computer to the controlled computers of the same channel;
- from controlled computers to the controlling computers of the same channel;
- never between two computers of the same role.

Field values are relayed unchanged; nrps sets the `origin` field to the sender's id.
Message types that only the server may send are not relayed.

A controlling computer that sends keystrokes or Control+Alt+Delete while no controlled
computer is in the channel hears "Remote NVDA not connected", at most once per second.

## Limits and behaviour

### Message size

Lines longer than `--max-line` (default 1 MiB), and messages with more than 64
top-level fields, are handled as follows:

- before the join, the connection is closed;
- after the join, the message is dropped and the client stays connected.

NVDA escapes non-ASCII text in JSON. As a result, a pushed clipboard larger than
roughly one million plain ASCII characters, about 170,000 accented, Cyrillic or Asian
characters, or about 85,000 emoji is silently not delivered, although NVDA still says
"clipboard pushed".

### Invalid messages

A line that is not a valid JSON object closes the connection, before or after the
join. Empty or blank lines are ignored.

### Connections

- `--max-connections` (default 32) limits the total number of simultaneous
  connections.
- `--max-per-ip` (default 8) limits connections per IPv4 address or per IPv6 /64
  prefix, because one IPv6 host usually controls a whole /64.
- Channel keys are limited to 256 bytes.

### Timeouts and keepalive

- The TLS handshake plus a valid `join` or `generate_key` must complete within
  `--auth-timeout` seconds (default 10).
- After joining there is no read timeout: a controlled computer can stay silent for
  days.
- On Linux, a peer that vanished without closing its connection (power cut, network
  loss) is removed after about 80 seconds.

### Outbound queues

Each client has its own outbound queue, limited in bytes by `--queue-bytes` (default 4
MiB). A full or stuck client never delays the sender, the other clients of its
channel, or any other channel.

A client is disconnected when the data waiting for it would exceed `--queue-bytes`, or
when writing one message to it takes longer than `--write-timeout` seconds (default
30). Normal NVDA traffic is far below 4 MiB.

### Memory use

Worst-case memory at the default settings is roughly 170 MiB: `--max-connections`
times `--queue-bytes` (128 MiB) of queued data, plus `--max-connections` times
`--max-line` (32 MiB) of read buffers, plus small TLS buffers.

## Logging

- Keys (channel names) and message contents are never logged.
- Joins are logged with the client id, the IP address and port, the role and a
  per-process random hash of the channel. Departures are logged with the client id
  and the reason.
- Nothing is logged when a TCP connection is merely accepted.
- Rejected connections, and connections that never join, are logged at most 20 lines
  per 10 seconds.
- With a self-signed certificate, each NVDA connection normally starts with a rejected
  handshake ("does not trust the certificate") and a certificate check (closed right
  after the handshake). Both are expected.
- The certificate fingerprint is logged at every start.

## Listening addresses

`--listen` defaults to `0.0.0.0:6837` and `[::]:6837`, and can be repeated. An IPv6
listener accepts only IPv6, so `--listen [::]:6837` alone refuses IPv4 clients.

`--port N` (1 to 65535, default 6837) changes the port of the default listeners
(`0.0.0.0:N` and `[::]:N`). It cannot be combined with `--listen`.

If IPv6 is unavailable on the server, the default IPv6 listener is skipped with a
warning. Any other address that cannot be opened (for example because another program
already uses the port) stops nrps with an error.

## Command-line options

`nrps --help` is the authoritative list of options and defaults. In summary:

- `--listen ADDR`, `--port N`: see [listening addresses](#listening-addresses).
- `--cert FILE`, `--key FILE`: the TLS certificate and private key in PEM format
  (default `cert.pem` and `key.pem` in the working directory). See [TLS
  certificate](deployment.md#tls-certificate).
- `--max-connections N`, `--max-per-ip N`: at least 1. See
  [connections](#connections).
- `--max-line BYTES`: at least 1024. See [message size](#message-size).
- `--auth-timeout SECS`, `--write-timeout SECS`, `--ping-interval SECS`: 1 to 86400
  seconds.
- `--queue-bytes BYTES`: must be at least `--max-line` plus 16384; nrps exits with an
  error otherwise.
- `--motd TEXT`: a message of the day that NVDA shows in a dialog after joining. See
  [settings](deployment.md#settings) before using it.
- `--version` prints the version.
