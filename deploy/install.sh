#!/usr/bin/env bash
# Install, upgrade or remove nrps (NVDA Remote private relay server) on Debian.
# Run as root from a folder that contains the nrps binary and nrps.service.
# See --help.
set -euo pipefail

DEFAULT_PORT=6837
BIN=/usr/local/bin/nrps
UNIT=/etc/systemd/system/nrps.service
DROPIN_DIR=/etc/systemd/system/nrps.service.d
DROPIN=$DROPIN_DIR/port.conf
SETTINGS=/etc/default/nrps
STATE=/var/lib/nrps
SVC_USER=nrps
U32_MAX=4294967295
QUEUE_MARGIN=16384

# Settings asked at install time, in this order, with nrps's defaults.
KEYS=(port ipv4_only max_connections max_per_ip max_line auth_timeout write_timeout
    ping_interval queue_bytes motd)
declare -A DEFAULTS=([port]=$DEFAULT_PORT [ipv4_only]=no [max_connections]=32
    [max_per_ip]=8 [max_line]=1048576 [auth_timeout]=10 [write_timeout]=30
    [ping_interval]=120 [queue_bytes]=4194304 [motd]="")
declare -A LABELS=([port]="Port" [ipv4_only]="IPv4 only"
    [max_connections]="Maximum connections"
    [max_per_ip]="Maximum connections per IP address or IPv6 /64"
    [max_line]="Maximum line length in bytes"
    [auth_timeout]="Seconds allowed for the TLS handshake and join"
    [write_timeout]="Seconds allowed to write one message to a client"
    [ping_interval]="Seconds between keepalive pings"
    [queue_bytes]="Bytes queued per client before it is disconnected"
    [motd]="Message of the day shown by NVDA after joining")
declare -A S
for k in "${KEYS[@]}"; do S[$k]=${DEFAULTS[$k]}; done

SRC_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# Journal lines older than this run are ignored when looking for the fingerprint.
START_EPOCH=$(date +%s)

usage() {
    cat <<EOF
Usage: sudo bash install.sh [--port N] [--yes] [--dry-run]
       sudo bash install.sh --uninstall [--purge] [--yes] [--dry-run]

Installs or upgrades nrps from the nrps binary and nrps.service found next to
this script: creates the system user nrps and $STATE (if missing),
installs $BIN, $UNIT and the settings file
$SETTINGS, enables and (re)starts the service, then prints the
service status and the certificate fingerprint. Running it again upgrades the
binary and the unit; $STATE (certificate and key) is never
touched, so the fingerprint stays the same.

On a first install the script asks each setting (port, IPv4 only, limits,
timeouts, message of the day). They are kept in $SETTINGS; on
later runs it shows them and asks whether to keep them (answer n to change
them one by one). Without a terminal, or with --yes, it asks nothing and uses
that file (or the defaults).

Options:
  --port N     TCP port nrps listens on (1024-65535, default $DEFAULT_PORT). Overrides
               the port stored in $SETTINGS.
  --uninstall  Stop and disable the service, remove the binary and the unit.
               Keeps $STATE, $SETTINGS and the nrps user.
  --purge      With --uninstall: also delete $STATE (certificate and
               private key), $SETTINGS, the drop-ins in $DROPIN_DIR
               and the nrps user and group.
               Asks for confirmation.
  -y, --yes    Ask no questions: keep the settings, and purge without asking.
  --dry-run    Print the commands that change the system instead of running
               them. Does not require root, and does not ask to confirm
               --purge.
  -h, --help   Show this help.

The script does not change the firewall: open the TCP port yourself.
EOF
}

say() { printf '%s\n' "$*"; }
warn() { printf 'Warning: %s\n' "$*" >&2; }
die() {
    printf 'Error: %s\n' "$*" >&2
    exit 1
}
have() { command -v "$1" >/dev/null 2>&1; }

# Run a command that changes the system, or only print it with --dry-run.
run() {
    if [[ $DRY_RUN == 1 ]]; then
        printf '+'
        printf ' %q' "$@"
        printf '\n'
    else
        "$@"
    fi
}

# A failed requirement: only a warning in a dry run.
require() {
    if ((DRY_RUN)); then warn "$1"; else die "${2:-$1}"; fi
}

# ---------------------------------------------------------------- arguments

ACTION=install
PURGE=0
YES=0
DRY_RUN=0
PORT=""

while (($#)); do
    case $1 in
    --port)
        [[ -n ${2:-} ]] || die "--port needs a value (see --help)"
        PORT=$2
        shift 2
        ;;
    --uninstall)
        ACTION=uninstall
        shift
        ;;
    --purge)
        PURGE=1
        shift
        ;;
    -y | --yes)
        YES=1
        shift
        ;;
    --dry-run)
        DRY_RUN=1
        shift
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        die "unknown option: $1 (see --help)"
        ;;
    esac
done

if ((PURGE)) && [[ $ACTION != uninstall ]]; then
    die "--purge is only valid with --uninstall"
fi
if [[ -n $PORT && $ACTION != install ]]; then
    die "--port is only valid when installing"
fi
# ------------------------------------------------------------ environment

[[ $(uname -s) == Linux ]] || die "this script runs on Linux only"
[[ $(uname -m) == x86_64 ]] || die "the nrps binary is built for x86_64; this machine is $(uname -m)"
[[ $(id -u) == 0 ]] || require "not running as root; dry run only, some checks may be incomplete" \
    "run this script as root, for example: sudo bash install.sh"
[[ -d /run/systemd/system ]] || require "systemd is not running on this machine"

if ((DRY_RUN)); then
    say "Dry run: commands that change the system are printed, not run."
fi

# ------------------------------------------------------------------ install

service_active() {
    systemctl is-active --quiet nrps 2>/dev/null
}

check_sources() {
    [[ -f $SRC_DIR/nrps ]] || die "nrps binary not found in $SRC_DIR"
    [[ -f $SRC_DIR/nrps.service ]] || die "nrps.service not found in $SRC_DIR"
    if [[ ! -x $SRC_DIR/nrps ]]; then
        # scp from Windows usually drops the executable bit.
        run chmod u+x "$SRC_DIR/nrps"
    fi
    if [[ -x $SRC_DIR/nrps ]]; then
        NEW_VERSION=$("$SRC_DIR/nrps" --version 2>&1) ||
            die "$SRC_DIR/nrps does not run on this machine: $NEW_VERSION"
        [[ $NEW_VERSION == nrps\ * ]] ||
            die "$SRC_DIR/nrps does not look like nrps (--version printed: $NEW_VERSION)"
    else
        NEW_VERSION="nrps (version not checked in dry run)"
    fi
    OLD_VERSION=""
    if [[ -x $BIN ]]; then
        OLD_VERSION=$("$BIN" --version 2>/dev/null || true)
    fi
}

# ----------------------------------------------------------------- settings

# Set MIN and MAX for numeric setting $1. Ranges are those of nrps --help.
limits() {
    case $1 in
    port) MIN=1024 MAX=65535 ;;
    max_connections | max_per_ip) MIN=1 MAX=$U32_MAX ;;
    max_line) MIN=1024 MAX=$((U32_MAX - QUEUE_MARGIN)) ;;
    queue_bytes) MIN=$((S[max_line] + QUEUE_MARGIN)) MAX=$U32_MAX ;;
    *) MIN=1 MAX=86400 ;;
    esac
}

# Set VALUE to yes or no for answer $1; fail if it is neither.
yes_no() {
    case ${1,,} in
    y | yes) VALUE=yes ;;
    n | no) VALUE=no ;;
    *) return 1 ;;
    esac
}

# Check value $2 for setting $1. On success set VALUE to the normalised value;
# on failure set REASON to a short sentence.
check_setting() {
    local key=$1 v=$2
    VALUE=$v
    REASON=""
    case $key in
    ipv4_only)
        yes_no "$v" || REASON="Answer y or n."
        ;;
    motd)
        if [[ $v == *[\"\'\\\$]* ]]; then
            REASON="The message cannot contain quotes, backslashes or dollar signs."
        elif [[ $v =~ [[:cntrl:]] ]]; then
            REASON="The message cannot contain control characters."
        elif [[ $v == -* ]]; then
            REASON="The message cannot start with a hyphen."
        fi
        ;;
    *)
        limits "$key"
        if [[ ! $v =~ ^[0-9]{1,10}$ ]] || ((10#$v < MIN || 10#$v > MAX)); then
            REASON="Enter a whole number from $MIN to $MAX."
            if [[ $key == queue_bytes ]]; then
                REASON="Enter a whole number from $MIN (maximum line length + $QUEUE_MARGIN) to $MAX."
            fi
        else
            VALUE=$((10#$v))
        fi
        ;;
    esac
    [[ -z $REASON ]]
}

# Read NRPS_OPTIONS from the settings file into S. The file is parsed, never
# sourced; only the options this script writes are accepted. When an option is
# repeated, the last one wins.
load_settings() {
    local line value key word
    local sq="^[[:space:]]*'(.*)'[[:space:]]*\$" dq='^[[:space:]]*"(.*)"[[:space:]]*$'
    local re='^[[:space:]]*--([a-z-]+)[[:space:]]+("([^"]*)"|([^[:space:]"]+))(.*)$'
    line=$(grep -E '^[[:space:]]*NRPS_OPTIONS=' "$SETTINGS" | tail -n 1) || true
    value=${line#*=}
    if [[ $value =~ $sq || $value =~ $dq ]]; then value=${BASH_REMATCH[1]}; fi
    [[ $value != *[\'\\]* ]] || die "$SETTINGS: NRPS_OPTIONS must not contain single quotes or backslashes"
    while [[ $value =~ [^[:space:]] ]]; do
        if [[ ! $value =~ $re ]] || [[ -n ${BASH_REMATCH[5]} && ${BASH_REMATCH[5]} != [[:space:]]* ]]; then
            die "$SETTINGS: cannot read NRPS_OPTIONS at: $value"
        fi
        key=${BASH_REMATCH[1]//-/_}
        word=${BASH_REMATCH[3]}${BASH_REMATCH[4]}
        value=${BASH_REMATCH[5]}
        if [[ $key == listen ]]; then
            [[ $word =~ ^0\.0\.0\.0:([0-9]+)$ ]] ||
                die "$SETTINGS: only --listen 0.0.0.0:PORT is supported, not --listen $word"
            S[ipv4_only]=yes
            key=port
            word=${BASH_REMATCH[1]}
        elif [[ $key == port ]]; then
            S[ipv4_only]=no
        elif [[ $key == ipv4_only || ! -v DEFAULTS[$key] ]]; then
            die "$SETTINGS: unknown option --${key//_/-}"
        fi
        S[$key]=$word
    done
    # Checked in KEYS order, so that --max-line is known before --queue-bytes.
    for key in "${KEYS[@]}"; do
        check_setting "$key" "${S[$key]}" ||
            die "$SETTINGS: invalid ${key//_/-} value \"${S[$key]}\": $REASON"
        S[$key]=$VALUE
    done
}

# NRPS_OPTIONS for the settings in S: only the options that differ from the
# defaults. systemd splits the value at spaces and respects double quotes.
settings_options() {
    local out=() key
    if [[ ${S[ipv4_only]} == yes ]]; then
        out+=(--listen "0.0.0.0:${S[port]}")
    elif [[ ${S[port]} != "$DEFAULT_PORT" ]]; then
        out+=(--port "${S[port]}")
    fi
    # The numeric settings: KEYS from max_connections to queue_bytes.
    for key in "${KEYS[@]:2:7}"; do
        if [[ ${S[$key]} != "${DEFAULTS[$key]}" ]]; then
            out+=("--${key//_/-}" "${S[$key]}")
        fi
    done
    if [[ -n ${S[motd]} ]]; then out+=(--motd "\"${S[motd]}\""); fi
    printf '%s\n' "${out[*]}"
}

print_settings() {
    local key
    for key in "${KEYS[@]}"; do say "  ${LABELS[$key]}: ${S[$key]:-none}"; done
}

# Ask for one setting until the answer is valid; Enter keeps the value shown.
ask_setting() {
    local key=$1 shown=${S[$1]} hint answer
    case $key in
    ipv4_only) hint=", no IPv6 listener (y/n)" ;;
    motd) hint=", - for none (recommended if a computer is unattended)" ;;
    *)
        limits "$key"
        # A larger maximum line length can make the current queue size too small.
        if ((shown < MIN)); then shown=$MIN; fi
        if ((MAX > 86400)); then hint=" (at least $MIN)"; else hint=" ($MIN-$MAX)"; fi
        ;;
    esac
    while true; do
        printf '%s%s [%s]: ' "${LABELS[$key]}" "$hint" "${shown:-none}"
        read -r answer || die "no answer; nothing was changed"
        [[ -n $answer ]] || answer=$shown
        if [[ $key == motd && ($answer == - || ${answer,,} == none) ]]; then answer=""; fi
        if check_setting "$key" "$answer"; then
            S[$key]=$VALUE
            return 0
        fi
        say "$REASON"
    done
}

ask_all() {
    local key
    say "Settings for nrps. Press Enter to keep the value in brackets."
    for key in "${KEYS[@]}"; do ask_setting "$key"; done
}

ask_yes_no() {
    local answer
    while true; do
        printf '%s [y]: ' "$1"
        read -r answer || die "no answer; nothing was changed"
        if [[ -z $answer ]]; then return 0; fi
        if yes_no "$answer"; then
            [[ $VALUE == yes ]]
            return
        fi
        say "Answer y or n."
    done
}

print_plan() {
    say ""
    say "This will:"
    if [[ -n $OLD_VERSION ]]; then
        say "- upgrade $BIN from $OLD_VERSION to $NEW_VERSION,"
    else
        say "- install $NEW_VERSION as $BIN,"
    fi
    getent passwd "$SVC_USER" >/dev/null || say "- create the system user $SVC_USER,"
    [[ -d $STATE ]] || say "- create $STATE for the certificate and key,"
    say "- install $UNIT,"
    if ((MIGRATE)); then
        say "- move the port from the old drop-in $DROPIN to $SETTINGS,"
    elif [[ -f $DROPIN ]]; then
        say "- remove the old drop-in $DROPIN,"
    fi
    say "- write the settings below to $SETTINGS,"
    say "- enable and (re)start the nrps service."
    say "Settings:"
    print_settings
}

# Settings come from the settings file, else the old port drop-in, else the
# defaults; --port overrides the port. With a terminal and without --yes,
# show them and offer to change them.
choose_settings() {
    local f p ask=1 interactive=0
    MIGRATE=0
    if [[ -f $SETTINGS ]]; then
        load_settings
        ask=0
    elif [[ -f $DROPIN ]]; then
        # Port of the drop-in written by older versions of this script.
        p=$(sed -n 's/^ExecStart=.* --port \([0-9]\{1,5\}\)$/\1/p' "$DROPIN" | tail -n 1)
        if [[ -n $p ]] && check_setting port "$p"; then
            S[port]=$VALUE
            MIGRATE=1
        fi
        ask=0
    fi
    OLD_PORT=${S[port]}
    if [[ -n $PORT ]]; then
        check_setting port "$PORT" || die "invalid port: $PORT (expected 1024-65535)"
        S[port]=$VALUE
    fi
    for f in "$DROPIN_DIR"/*.conf; do
        if [[ $f != "$DROPIN" ]] && grep -qs '^ExecStart=' "$f"; then
            warn "$f sets ExecStart, so $SETTINGS may be ignored"
        fi
    done
    if [[ -t 0 ]] && ((!YES)); then interactive=1; else ask=0; fi
    while true; do
        if ((ask)); then ask_all; fi
        print_plan
        ((interactive)) || break
        if ask_yes_no "Use these settings and continue? Answer n to change them, or press Ctrl+C to cancel."; then
            break
        fi
        ask=1
    done
    PORT=${S[port]}
    say ""
}

check_port_free() {
    service_active && [[ $PORT == "$OLD_PORT" ]] && return 0
    have ss || return 0
    local listeners
    listeners=$(ss -Hltn "sport = :$PORT" 2>/dev/null || true)
    if [[ -n $listeners ]]; then
        printf '%s\n' "$listeners" >&2
        die "another program already listens on port $PORT. If it is an older relay,
stop and disable it first (systemctl disable --now OLD-UNIT-NAME), then run this
script again. Check with: ss -ltnp 'sport = :$PORT'"
    fi
}

ensure_user() {
    if ! getent passwd "$SVC_USER" >/dev/null; then
        local gid_opt=()
        if getent group "$SVC_USER" >/dev/null; then
            gid_opt=(--gid "$SVC_USER")
        fi
        run useradd --system "${gid_opt[@]}" --home-dir "$STATE" --no-create-home \
            --shell /usr/sbin/nologin "$SVC_USER"
    fi
    if [[ ! -d $STATE ]]; then
        run install -d -o "$SVC_USER" -g "$SVC_USER" -m 0700 "$STATE"
    fi
}

install_files() {
    # Install under a temporary name, check it runs, then rename it into place.
    trap 'rm -f "$BIN.new"' EXIT
    run install -m 0755 "$SRC_DIR/nrps" "$BIN.new"
    run "$BIN.new" --version
    run mv -f "$BIN.new" "$BIN"
    trap - EXIT
    run install -m 0644 "$SRC_DIR/nrps.service" "$UNIT"
    write_settings
    if [[ -f $DROPIN ]]; then
        run rm -f "$DROPIN"
        run rmdir --ignore-fail-on-non-empty "$DROPIN_DIR"
    fi
}

# The unit reads NRPS_OPTIONS from the settings file.
write_settings() {
    local content
    content="# Settings for nrps, read by $UNIT.
# Written by install.sh: run it again to change them. Options not given here
# use the defaults of nrps --help.
NRPS_OPTIONS='$(settings_options)'"
    if ((DRY_RUN)); then
        say "+ write $SETTINGS:"
        printf '%s\n' "$content" | sed 's/^/    /'
    else
        printf '%s\n' "$content" >"$SETTINGS.new"
        chmod 0644 "$SETTINGS.new"
        mv -f "$SETTINGS.new" "$SETTINGS"
    fi
}

start_service() {
    local was_active=0
    service_active && was_active=1
    run systemctl daemon-reload
    run systemctl enable --now nrps
    if ((was_active)); then
        run systemctl restart nrps
    fi
}

fingerprint_from_journal() {
    journalctl -u nrps --since "@$START_EPOCH" -o cat --no-pager 2>/dev/null |
        sed -n 's/^certificate SHA-256 fingerprint: \([0-9a-f]\{64\}\)$/\1/p' |
        tail -n 1 || true
}

report_service() {
    local cert_existed=$1 state fp="" i
    # Wait for the service and the fingerprint line (up to 15 seconds).
    for ((i = 0; i < 15; i++)); do
        if service_active && [[ -f $STATE/cert.pem && -n $(fingerprint_from_journal) ]]; then
            break
        fi
        sleep 1
    done
    state=$(systemctl is-active nrps 2>/dev/null || true)
    if [[ -f $STATE/cert.pem ]] && have openssl; then
        fp=$(openssl x509 -in "$STATE/cert.pem" -outform DER | sha256sum | cut -d ' ' -f 1) || fp=""
    fi
    [[ -n $fp ]] || fp=$(fingerprint_from_journal)

    say ""
    say "Service status: $state"
    if [[ $state != active ]]; then
        say "nrps is not running. See: sudo journalctl -u nrps -n 50"
    fi
    if [[ -n $fp ]]; then
        say "Certificate SHA-256 fingerprint: $fp"
    else
        say "Certificate fingerprint not found yet. Print it later with:"
        say "  sudo journalctl -u nrps | grep fingerprint"
    fi
    if ((cert_existed)); then
        say "The existing certificate was kept, so the fingerprint is unchanged."
    elif [[ -f $STATE/cert.pem ]]; then
        say "A new certificate was generated. Record the fingerprint in a text file on"
        say "your own computer, and back up $STATE/cert.pem and"
        say "$STATE/key.pem to a safe place (the key is secret)."
    fi
}

next_steps() {
    local ip_versions="IPv4 and IPv6" suffix="" example=""
    [[ ${S[ipv4_only]} == yes ]] && ip_versions="IPv4"
    if [[ $PORT != "$DEFAULT_PORT" ]]; then
        suffix=", followed by :$PORT"
        example=", for example [2001:db8::1]:$PORT"
    fi
    cat <<EOF

Firewall: this script does not change it. Open TCP port $PORT inbound, for
$ip_versions, ideally only from the addresses of your own NVDA machines.

Next steps in NVDA (version 2026.1.1 or later on every machine):
1. Host: this server's IP address or a domain name you control$suffix.
   An IPv6 address goes in brackets$example.
2. Key: the same long random key on both machines, for example the output of
   openssl rand -hex 16. Do not use NVDA's "Generate key" button.
3. At the first connection on each machine, compare the fingerprint in NVDA's
   Security Warning with the one above, character by character, before
   choosing "Connect and do not ask again for this server".
EOF
}

do_install() {
    local cert_existed=0
    check_sources
    choose_settings
    check_port_free
    [[ -f $STATE/cert.pem ]] && cert_existed=1
    ensure_user
    install_files
    start_service
    if ((DRY_RUN)); then
        say "Dry run: not checking the service."
    else
        report_service "$cert_existed"
    fi
    next_steps
}

# ---------------------------------------------------------------- uninstall

confirm_purge() {
    ((YES || DRY_RUN)) && return 0
    say "--purge deletes $STATE, including the certificate and its private key."
    say "Every NVDA machine will then see a new fingerprint after a reinstall, and"
    say "an unattended computer stays blocked on the Security Warning until someone answers it."
    if ! { true </dev/tty; } 2>/dev/null; then
        die "no terminal to confirm; add --yes to purge without asking"
    fi
    local answer
    printf 'Type yes to delete them: ' >/dev/tty
    read -r answer </dev/tty || answer=""
    [[ $answer == yes ]] || die "purge cancelled; nothing was changed"
}

do_uninstall() {
    local had_user=0
    ((PURGE)) && confirm_purge
    if [[ -f $UNIT ]] || systemctl cat nrps >/dev/null 2>&1; then
        run systemctl disable --now nrps
    fi
    run rm -f "$BIN" "$UNIT" "$DROPIN"
    if [[ -d $DROPIN_DIR ]]; then run rmdir --ignore-fail-on-non-empty "$DROPIN_DIR"; fi
    run systemctl daemon-reload
    if systemctl is-failed --quiet nrps 2>/dev/null; then
        run systemctl reset-failed nrps
    fi
    if ((PURGE)); then
        run rm -rf "$STATE" "$SETTINGS" "$DROPIN_DIR"
        if getent passwd "$SVC_USER" >/dev/null; then
            had_user=1
            run userdel "$SVC_USER"
        fi
        # userdel normally deletes the group too, which a dry run cannot see.
        if ! ((DRY_RUN && had_user)) && getent group "$SVC_USER" >/dev/null; then
            run groupdel "$SVC_USER"
        fi
        say "nrps removed, including $STATE, $SETTINGS and the $SVC_USER user."
    else
        say "nrps removed. Kept $STATE (certificate and key), $SETTINGS"
        say "and the $SVC_USER user; add --purge to delete them too."
    fi
}

case $ACTION in
install) do_install ;;
uninstall) do_uninstall ;;
esac
