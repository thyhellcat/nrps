# nrps - NVDA Remote private server

nrps is a small, self-hosted TLS relay server for NVDA's built-in Remote Access feature.
It is meant for people who want their own private relay instead of a public one.

A typical use is a controlled computer that stays connected at all times, and a
controlling computer that connects to it when needed, both using the same long key.
Several independent channels (keys) can share one server.

## Requirements

- A Linux server reachable from your NVDA computers, and an account with sudo
  (administrative) rights on it. The install script targets Debian with systemd on
  x86_64.
- NVDA 2026.1.1 or later on every computer that connects to the server. Earlier
  versions do not protect reconnections; see [why NVDA 2026.1.1 is
  required](SECURITY.md#required-nvda-version).

## Quick start

1. On the server, download and unpack the latest release, which contains the static
   binary, the install script and the systemd unit:

   ```bash
   curl -LO https://github.com/thyhellcat/nrps/releases/latest/download/nrps-x86_64-linux.tar.gz
   tar xzf nrps-x86_64-linux.tar.gz
   cd nrps-x86_64-linux
   ```

   To build it yourself instead, see [building nrps](docs/building.md).
2. Run `sudo bash install.sh` in that folder. It asks for the port (6837 by default)
   and the other settings; Enter keeps the value shown. See [installing on
   Debian](docs/deployment.md#installing-with-the-script).
3. Allow inbound TCP port 6837 (or your port) in the server's firewall and in your
   hosting provider's firewall. See [firewall advice](docs/deployment.md#firewall).
4. Record the certificate fingerprint that the script prints, and back up the
   certificate and key from `/var/lib/nrps`.
5. In NVDA on each computer, enable Remote Access and connect with "Use existing"
   server, the server's address as Host (followed by `:PORT` if you changed the port),
   and a long random key that you generate yourself, not with NVDA's "Generate key"
   button. See [configuring NVDA](docs/nvda-setup.md).
6. At the first connection, compare the fingerprint in NVDA's Security Warning with
   the one you recorded, character by character. If the warning ever appears again,
   choose Cancel; see [if the Security Warning appears
   again](docs/nvda-setup.md#if-the-security-warning-appears-again).

## Documentation

- [Configuring NVDA](docs/nvda-setup.md): key, host, first connection, fingerprint
  checks, automatic connection.
- [Deploying the server](docs/deployment.md): install script, settings, upgrading,
  uninstalling, manual installation, firewall, logs, certificate.
- [Design and limits](docs/design.md): protocol, routing, limits, logging and
  command-line options.
- [Building nrps](docs/building.md): static musl build from Linux or Windows, tests.
- [Security policy](SECURITY.md): threat model, required NVDA version, reporting a
  vulnerability.

## Licence

nrps is released under the MIT licence. See [the LICENSE file](LICENSE).
