# Configuring NVDA for nrps

This guide uses "the controlled computer" for the computer that is controlled, and
"the controlling computer" for the computer that controls it.

All steps are for NVDA 2026.1.1 or later, on every computer; see [why NVDA 2026.1.1
is required](../SECURITY.md#required-nvda-version).

Before you start, you need the server's certificate fingerprint. The install script
prints it; see [recording the fingerprint](deployment.md#recording-the-fingerprint).

## Choosing the key and the host

1. Generate a long random key yourself, for example on the server with
   `openssl rand -hex 16` (32 characters; nrps accepts up to 256 bytes). Do not use
   NVDA's "Generate key" button with this server: the connection that receives that
   key is not protected by the pinned fingerprint.
2. For the Host, prefer the server's IP address, or a domain name that you control
   (not a free dynamic DNS name). Use exactly the same Host text everywhere: NVDA
   remembers the trusted fingerprint under that exact text. NVDA has no Port field
   for "Use existing", so append `:port` if the port is not 6837, for example
   `server.example.org:7000`. An IPv6 address must be in brackets, for example
   `[2001:db8::1]` or `[2001:db8::1]:7000`.
3. NVDA tries only the first address of a name. If the name has an IPv6 (AAAA)
   record, IPv6 must work from every computer; otherwise use the IPv4 address.

## First connection on each computer

1. Enable Remote Access: NVDA menu, Preferences, Settings, Remote Access category,
   check "Enable Remote Access", then OK. The Remote Access items in the Tools menu
   appear only after this.
2. NVDA menu, Tools, Remote Access, Connect. Mode: "Allow this computer to be
   controlled" on the controlled computer, "Control another computer" on the
   controlling computer. Server: "Use existing". Host and Key as chosen above.
3. NVDA shows a Security Warning with the server's SHA-256 fingerprint. Compare it
   with the fingerprint you recorded, character by character. Only if they match
   exactly, choose "Connect and do not ask again for this server". If the fingerprint
   is "None", or differs, choose Cancel.
4. If no Security Warning appears at all on a computer that never connected before,
   disconnect: NVDA accepted a certificate trusted by Windows instead of yours. See
   [networks that inspect TLS traffic](#networks-that-inspect-tls-traffic).
5. Save the configuration with NVDA+Control+C. Otherwise NVDA writes settings and
   trusted fingerprints to disk only when it exits normally.

## The controlling computer

Connect when needed with NVDA menu, Tools, Remote Access, Connect, in "Control another
computer" mode, with the same Host and key. Press NVDA+Alt+Tab to switch the keyboard
between the remote and the local computer.

## If the Security Warning appears again

After the first connection, the Security Warning must never appear again on that
computer. NVDA shows exactly the same dialog when the server's certificate has
changed, with nothing indicating that a different certificate was trusted before. A
changed certificate may mean that someone is impersonating your server to steal the
key.

Every time the warning appears, compare the fingerprint with the server's value. Never
accept a changed fingerprint unless you regenerated the server certificate yourself.
Choose Cancel and check the server. This command shows whether nrps created a new
certificate:

```bash
sudo journalctl -u nrps | grep -E 'generated a new|fingerprint'
```

## Changing the certificate

If you deliberately replaced the server certificate:

1. On each computer, NVDA menu, Preferences, Settings, Remote Access category, press
   "Delete all trusted fingerprints" and confirm. This forgets every trusted Remote
   Access server, not only this one.
2. Connect manually once and accept the new fingerprint as described in [first
   connection on each computer](#first-connection-on-each-computer).
3. Save with NVDA+Control+C.

## Automatic connection on the controlled computer

1. NVDA menu, Preferences, Settings, Remote Access category. Check "Automatically
   connect after NVDA starts". Mode: "Allow this computer to be controlled". Server:
   "Use existing". Host: the same text as for the first connection. Key: your key.
   Then OK.
2. Press NVDA+Control+C to save.
3. Restart NVDA and check that it connects without any Security Warning. A warning at
   this point would block an unattended computer until someone answers it in person.

## Sign-in screen after a reboot

The automatic connection runs in your Windows session. After a reboot, until someone
signs in, the controlled computer is not reachable unless NVDA also connects from the
sign-in screen.

To make it reachable there, in NVDA's General settings make sure NVDA starts during
sign-in, then press "Use currently saved settings during sign-in and on secure
screens". NVDA copies your configuration, including the key in plain text, to
`C:\Program Files\NVDA\systemConfig\nvda.ini`. This has two drawbacks:

- That copy is readable by every local account and by sandboxed apps on the computer.
  Anyone or anything able to read it gets the key and full remote control. Use this
  only on a computer whose other accounts and installed software you trust.
- The copy is not updated when you change the key later. Press the button again after
  every key change; until then the old key remains valid at the sign-in screen.

If you leave it off, the computer is reachable only after someone signs in.

## Networks that inspect TLS traffic

This applies to every computer, controlled and controlling. Avoid networks that
inspect TLS traffic (some corporate or school networks), and any computer that trusts
such a network's certificate authority. NVDA accepts any certificate that Windows
trusts for the Host, without checking your pinned fingerprint and without a warning.

## NVDA's log contains the key

At its default logging level (Info), NVDA writes the key in clear text to its log
(`%TEMP%\nvda.log`, then `nvda-old.log`) at every connection, on every computer, in a
line starting with "Connecting to". nrps cannot prevent this.

- Never share an NVDA log without first removing that line. If a log with the key was
  ever shared, choose a new key on every computer.
- On an unattended controlled computer, consider setting the logging level to
  "Disabled" (NVDA General settings, Logging level). Every other level, including
  "Debug warning", still logs the key.
