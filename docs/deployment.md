# Deploying nrps

This guide covers installing nrps as a systemd service on a Debian server, keeping it up
to date, the firewall, logs, and the TLS certificate. To build the binary first, see
[building nrps](building.md).

## Installing with the script

The script `install.sh` (`deploy/install.sh` in the repository) installs nrps as a
systemd service from the unit `nrps.service`. It needs Linux x86_64 with systemd and must be run with sudo.

1. If an older relay runs on the server, stop and disable it first. The script refuses
   to install while another program listens on the chosen port. If it is the
   community Go server, see also [migrating from the Go
   server](#migrating-from-the-go-server).

   ```bash
   sudo systemctl disable --now OLD-UNIT-NAME
   ```

2. On the server, download and unpack the latest release. Its folder contains the
   binary (`nrps`), the script and the unit:

   ```bash
   curl -LO https://github.com/thyhellcat/nrps/releases/latest/download/nrps-x86_64-linux.tar.gz
   ```

   Optionally, check the download with its checksum file:

   ```bash
   curl -LO https://github.com/thyhellcat/nrps/releases/latest/download/nrps-x86_64-linux.tar.gz.sha256
   sha256sum -c nrps-x86_64-linux.tar.gz.sha256
   ```

   Then unpack it:

   ```bash
   tar xzf nrps-x86_64-linux.tar.gz
   cd nrps-x86_64-linux
   ```

   If you built nrps yourself, copy the binary, `deploy/install.sh` and
   `deploy/nrps.service` to one folder on the server instead.

3. In that folder:

   ```bash
   sudo bash install.sh
   ```

   The script asks for the settings one by one (see [settings](#settings)), prints
   what it will do and asks for confirmation. It then creates the system user `nrps`
   and `/var/lib/nrps` (mode 0700), installs `/usr/local/bin/nrps`,
   `/etc/systemd/system/nrps.service` and the settings file `/etc/default/nrps`,
   enables and starts the service, then prints the service status and the
   certificate fingerprint. The script does not change the firewall; see
   [firewall](#firewall).

4. Record the fingerprint, as described in [recording the
   fingerprint](#recording-the-fingerprint).

5. Back up the certificate and key, as described in [backing up the
   certificate](#backing-up-the-certificate).

### Script options

- `--port N` uses TCP port N (1024 to 65535) instead of the stored port or 6837.
- `--dry-run` prints the commands that change the system instead of running them. It
  does not require root, and does not ask to confirm `--purge`.
- `--uninstall` removes nrps; see [uninstalling](#uninstalling).
- `--purge`, with `--uninstall`, also deletes the state folder, the settings file, the
  unit's drop-ins (`sudo systemctl edit nrps`) and the user.
- `-y` or `--yes` asks no questions: the script keeps the stored settings (or the
  defaults), and `--purge` deletes without asking. Without a terminal on standard
  input, the settings are not asked either; `--purge` still asks on the terminal, or
  needs `--yes`.
- `-h` or `--help` shows all options.

### Settings

The script asks, one question per line, for the port, IPv4 only, the connection
limits, the maximum line length, the timeouts, the ping interval, the queue size per
client and the message of the day: all options of `nrps --help` except `--cert`,
`--key` and `--listen`. Each question shows the current value in brackets; press
Enter to keep it. An invalid answer is explained and asked again.

Answering `y` to "IPv4 only" makes nrps listen on IPv4 only (`--listen 0.0.0.0:PORT`
instead of `--port PORT`).

For the message of the day, answer `-` or `none` for none. The message cannot contain
quotes, backslashes, dollar signs or control characters, nor start with a hyphen.
Leave it unset when a controlled computer is unattended: NVDA shows the message in a
modal dialog on every connected computer, including the unattended one, where it
stays open until someone closes it. NVDA shows it again whenever the text changes.

The answers are stored in `/etc/default/nrps`, for example:

```bash
NRPS_OPTIONS='--port 7000 --motd "Welcome"'
```

The unit reads this file and adds `NRPS_OPTIONS` to the nrps command line. Only the
options that differ from the defaults are written. To change a setting, run the
script again, or edit the line by hand with the same options and run `sudo systemctl
restart nrps`. The script stops with an error if it finds an option in the file that
it does not know.

With another port than 6837, the Host in NVDA is `server:N` instead of `server`.
Ports below 1024 are not possible: the service runs without privileges.

## Upgrading

Download and unpack the new release as in [installing with the
script](#installing-with-the-script), and run `sudo bash install.sh` again in its
folder. It shows the stored settings and asks one question:
answer `y` (or press Enter) to keep them, `n` to change them. It then replaces the
binary and the unit and restarts nrps; the NVDA computers reconnect. The certificate
in `/var/lib/nrps` is kept, so the fingerprint does not change.

## Uninstalling

`sudo bash install.sh --uninstall` stops and disables nrps and removes the binary and
the unit, but keeps `/var/lib/nrps`, `/etc/default/nrps` and the `nrps` user.

Add `--purge` to delete them too, along with any drop-in made with `sudo systemctl
edit nrps`. This destroys the certificate and its private key:
after a reinstall, every NVDA computer sees a new fingerprint; see [backing up the
certificate](#backing-up-the-certificate).

## Manual installation

Without the script, on the server:

1. Install the binary, create the user and the state folder:

   ```bash
   sudo install -m 0755 nrps /usr/local/bin/nrps
   sudo useradd --system --home-dir /var/lib/nrps --no-create-home --shell /usr/sbin/nologin nrps
   sudo install -d -o nrps -g nrps -m 0700 /var/lib/nrps
   ```

2. Install the unit and start it:

   ```bash
   sudo install -m 0644 nrps.service /etc/systemd/system/nrps.service
   sudo systemctl daemon-reload
   sudo systemctl enable --now nrps
   sudo journalctl -u nrps -n 20
   ```

   Settings are optional: to set some, put an `NRPS_OPTIONS` line in
   `/etc/default/nrps` as described in [settings](#settings) and restart nrps.

3. Record the fingerprint and back up the certificate as described below.

## Recording the fingerprint

nrps prints the certificate's SHA-256 fingerprint in its log at every start, and the
install script prints it at the end. Record it in a text file that you keep on your
own computer: you compare it with NVDA's Security Warning at the first connection on
each computer.

This command prints it again at any time, in the same format as NVDA (ignore the
trailing `-`):

```bash
sudo openssl x509 -in /var/lib/nrps/cert.pem -outform DER | sha256sum
```

## Backing up the certificate

Back up `/var/lib/nrps/cert.pem` and `/var/lib/nrps/key.pem` to a safe place. The key
is secret.

If the server is rebuilt, restore both files before the first start of nrps, with
owner `nrps` and mode 0600 for the key. If both files are missing, nrps silently
creates a new certificate, and every NVDA computer then shows the Security Warning
again; an unattended computer stays blocked on it until someone answers it in person.

If only one of the two files exists, nrps refuses to start. Restore the missing file
from your backup rather than deleting the other one.

## The systemd unit

The unit `nrps.service` runs nrps as the `nrps` user with the certificate and
key in `/var/lib/nrps`, restarts it if it stops, sandboxes it, and limits its
resources:

- `LimitNOFILE=4096`: keep it well above `--max-connections`.
- `MemoryMax=256M`: above the worst case of roughly 170 MiB at the default limits
  (see [memory use](design.md#memory-use)). If you raise `--max-connections`,
  `--queue-bytes` or `--max-line`, raise `MemoryMax` with them.
- `TasksMax=64`: raise it on servers with many CPU cores.

To change one of these values, run `sudo systemctl edit nrps` and set it under
`[Service]`, for example `MemoryMax=512M`, then `sudo systemctl restart nrps`. This
drop-in survives upgrades, whereas the script replaces the unit file. Do not set
`ExecStart` in a drop-in: it would replace the command line, so `/etc/default/nrps`
would be ignored. The script warns when it finds one.

## Firewall

Allow inbound TCP port 6837 (or the port you chose) to the server, over IPv4 and
IPv6, in your firewall and in your hosting provider's firewall if it has one.
Neither the script nor nrps changes the firewall.

If your NVDA computers have stable public addresses, allow only those as sources (for
IPv6, your /64 prefix) instead of the whole internet. This removes most of the
exposure to anonymous attackers: key guessing, connection flooding and memory
exhaustion; see [what nrps does not protect
against](../SECURITY.md#what-nrps-does-not-protect-against).

## Logs

nrps logs to standard error, which systemd stores in the journal:

```bash
sudo journalctl -u nrps -n 50
```

The journal keeps the IP addresses and connection times of your NVDA computers. To
limit how long they are kept, set `MaxRetentionSec=` (for example `1month`) in
`/etc/systemd/journald.conf`. See [logging](design.md#logging) for what nrps logs.

## TLS certificate

### Self-signed certificate

If neither the `--cert` nor the `--key` file exists, nrps generates a self-signed
certificate once. It is reused on later starts, so the fingerprint stays stable. The
SHA-256 fingerprint is printed at every start in NVDA's format (64 lowercase
hexadecimal characters). `--cert` and `--key` must be two different files.

The key file is created with mode 0600, and nrps logs a warning at start if the key
file is readable or writable by other users.

### Why not Let's Encrypt

Use the generated self-signed certificate. With a certificate from a public authority
such as Let's Encrypt, NVDA trusts any certificate a public authority issues for that
name and ignores your pinned fingerprint. nrps also reads the certificate only at
start, so it would have to be restarted after every renewal.

### Migrating from the Go server

nrps accepts ECDSA P-256 or P-384, RSA and Ed25519 keys. Certificates from the
community Go server use ECDSA P-521 keys and cannot be loaded. When moving from that
server, let nrps generate a new certificate, and accept the new fingerprint once on
each computer, as described in [changing the
certificate](nvda-setup.md#changing-the-certificate).
