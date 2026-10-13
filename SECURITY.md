# Security policy

## Threat model

The key (channel name) is the only secret. Anyone who knows it can join the channel and
fully control the controlled computer. There is no separate password or channel lock.

The server's certificate fingerprint, pinned by NVDA at the first connection, is what
proves to NVDA that it is talking to your server and not to an impostor who would
receive the key.

## Required NVDA version

Use NVDA 2026.1.1 or later on every computer that connects to an nrps server. Check
the version with NVDA menu, Help, About.

NVDA 2025.1 to 2026.1.0 have a security flaw (advisory GHSA-m268-mc77-j2cr, rated
critical): they check the pinned certificate fingerprint only when a connection is
first made. Every later automatic reconnect accepts any certificate without asking.
After any network drop or server restart, whoever answers for the server's address
receives your key and can control the controlled computer, with no warning. That can
be someone who hijacks DNS, takes over an expired dynamic DNS name, or sits on the
network path.

Update an unattended controlled computer deliberately: nobody sitting in front of it
will do it for you.

## What nrps protects against

- Eavesdropping: all traffic is encrypted with TLS, and NVDA checks the server's
  pinned self-signed certificate.
- Leaks through the server's logs: keys and message contents are never logged.
- Resource exhaustion: nrps limits connections in total and per address, line length,
  per-client queues and authentication time, and the systemd unit caps memory and
  tasks. See [limits and behaviour](docs/design.md#limits-and-behaviour).

## What nrps does not protect against

- Anyone who knows the key. Keep it long and random, and never share an NVDA log that
  contains it. See [NVDA's log contains the
  key](docs/nvda-setup.md#nvdas-log-contains-the-key).
- NVDA versions before 2026.1.1, as described above.
- A user who accepts a changed fingerprint. See [if the Security Warning appears
  again](docs/nvda-setup.md#if-the-security-warning-appears-again).
- A certificate from a public authority such as Let's Encrypt. NVDA then trusts any
  certificate a public authority issues for that name and ignores your pinned
  fingerprint. Use the self-signed certificate that nrps generates; see [why not
  Let's Encrypt](docs/deployment.md#why-not-lets-encrypt).
- Networks that inspect TLS traffic, and computers that trust their certificate
  authority. See [networks that inspect TLS
  traffic](docs/nvda-setup.md#networks-that-inspect-tls-traffic).
- Key guessing and connection flooding by anonymous attackers, beyond the limits
  above. An attacker who owns a larger IPv6 block than a /64 is not stopped by the
  per-address limit. Restricting source addresses in the firewall is the real
  protection; see [firewall advice](docs/deployment.md#firewall).
- Someone who can read the server's private key, or the copy of NVDA's configuration
  used at the sign-in screen. See [backing up the
  certificate](docs/deployment.md#backing-up-the-certificate) and [sign-in screen
  after a reboot](docs/nvda-setup.md#sign-in-screen-after-a-reboot).

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's private vulnerability
reporting: open the Security tab of this repository and choose "Report a
vulnerability". Do not open a public issue for a security problem.
