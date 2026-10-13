//! nrps: a small private relay server for NVDA Remote Access.
//!
//! Wire protocol (same as NVDA's built-in local relay, source/_remoteClient/server.py):
//! TLS, newline-delimited JSON objects. Before joining, a client may send
//! `protocol_version`, then either `join` {channel, connection_type} or `generate_key`.
//! After joining, every message from a controller (`master`) is forwarded to the
//! controlled machines (`slave`) of the channel and vice versa, with an `origin` field
//! set to the sender id. Message values are relayed byte for byte (see [`json`]).

mod json;

use std::collections::HashMap;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::hash::{BuildHasher, RandomState};
use std::io::{self, Write};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};
use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::time::{Instant, timeout, timeout_at};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::crypto::ring::default_provider;
use tokio_rustls::rustls::pki_types::pem::{self, PemObject};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::server::NoServerSessionStorage;
use tokio_rustls::rustls::{self, AlertDescription, ServerConfig};

use json::Field;

/// Writes one line to stderr; a failing stderr must never take the server down.
macro_rules! log {
    ($($t:tt)*) => {{
        let _ = writeln!(std::io::stderr().lock(), $($t)*);
    }};
}

/// How much larger than `max_line` a client's queue must at least be, so a maximal
/// message plus a few small ones always fit in an empty queue.
pub const QUEUE_MARGIN: usize = 16 << 10;

/// Longest accepted channel (key), in bytes. NVDA keys are 7 to about 32 characters.
pub const MAX_CHANNEL: usize = 256;

/// Runtime limits and options.
#[derive(Clone, Debug)]
pub struct Config {
    pub max_connections: usize,
    /// Per IPv4 address, or per IPv6 /64 prefix.
    pub max_per_ip: usize,
    /// Maximum length of one JSON line, in bytes. Longer lines close the connection
    /// before joining and are dropped (not relayed) after joining.
    pub max_line: usize,
    /// Deadline for the TLS handshake plus a valid `join` / `generate_key`.
    pub auth_timeout: Duration,
    /// Time allowed for writing one message to a client.
    pub write_timeout: Duration,
    pub ping_interval: Duration,
    /// Bytes that may wait in one client's outbound queue; a client whose queue would
    /// overflow is disconnected. At least `max_line` + [`QUEUE_MARGIN`] (raised if lower).
    /// Queued memory is bounded by `max_connections` x `queue_bytes`.
    pub queue_bytes: usize,
    pub motd: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_connections: 32,
            max_per_ip: 8,
            max_line: 1 << 20,
            auth_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(30),
            ping_interval: Duration::from_secs(120),
            queue_bytes: 4 << 20,
            motd: None,
        }
    }
}

impl Config {
    /// Clamps values that would otherwise panic (zero periods, Instant overflow).
    fn sanitized(mut self) -> Self {
        let day = Duration::from_secs(86_400);
        let clamp = |d: Duration| d.clamp(Duration::from_millis(1), day);
        self.auth_timeout = clamp(self.auth_timeout);
        self.write_timeout = clamp(self.write_timeout);
        self.ping_interval = clamp(self.ping_interval);
        self.max_line = self.max_line.clamp(1, Semaphore::MAX_PERMITS / 2);
        self.queue_bytes = self
            .queue_bytes
            .clamp(self.max_line + QUEUE_MARGIN, Semaphore::MAX_PERMITS);
        self
    }
}

/// Loads the TLS certificate and key, generating a self-signed pair if neither exists.
/// Returns the acceptor and the SHA-256 fingerprint of the certificate (hex, as NVDA shows it).
pub fn load_or_create_tls(
    cert: &Path,
    key: &Path,
) -> Result<(TlsAcceptor, String), Box<dyn Error>> {
    let exists = |p: &Path| {
        p.try_exists()
            .map_err(|e| format!("cannot access {}: {e}", p.display()))
    };
    if cert == key
        || (exists(cert)? && exists(key)? && fs::canonicalize(cert)? == fs::canonicalize(key)?)
    {
        return Err("the certificate and key must be two different files".into());
    }
    match (exists(cert)?, exists(key)?) {
        (true, true) => {}
        (false, false) => {
            generate(cert, key)?;
            log!(
                "generated a new self-signed certificate in {}",
                cert.display()
            );
        }
        _ => {
            return Err(format!(
                "only one of {} and {} exists: restore the missing file from a backup, or \
                 delete both to generate a new certificate (NVDA will then show a new fingerprint)",
                cert.display(),
                key.display()
            )
            .into());
        }
    }
    let certs = CertificateDer::pem_file_iter(cert)
        .and_then(|it| it.collect::<Result<Vec<_>, _>>())
        .map_err(|e| pem_error(cert, e))?;
    let first = certs
        .first()
        .ok_or_else(|| format!("no certificate in {}", cert.display()))?;
    let digest = ring::digest::digest(&ring::digest::SHA256, first);
    let fingerprint = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    let private = PrivateKeyDer::from_pem_file(key).map_err(|e| pem_error(key, e))?;
    warn_if_exposed(key);
    let mut config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, private)
        .map_err(|e| {
            format!(
                "{} / {} rejected: {e} (supported keys: ECDSA P-256 or P-384, RSA, Ed25519)",
                cert.display(),
                key.display()
            )
        })?;
    // NVDA never resumes sessions: keep no resumption secrets in memory.
    config.session_storage = Arc::new(NoServerSessionStorage {});
    config.send_tls13_tickets = 0;
    Ok((TlsAcceptor::from(Arc::new(config)), fingerprint))
}

/// Never includes file contents: PEM errors can carry raw key bytes.
fn pem_error(path: &Path, e: pem::Error) -> String {
    match e {
        pem::Error::Io(e) => format!("cannot read {}: {e}", path.display()),
        _ => format!("{} is not a valid PEM file", path.display()),
    }
}

fn generate(cert: &Path, key: &Path) -> Result<(), Box<dyn Error>> {
    use rcgen::{
        CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose,
    };
    let pair = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec!["nrps".to_owned()])?;
    params.distinguished_name = DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, "nrps");
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let issued = params.self_signed(&pair)?;
    write_new(key, pair.serialize_pem().as_bytes(), 0o600)?;
    write_new(cert, issued.pem().as_bytes(), 0o644)?;
    Ok(())
}

/// Writes a complete, synced file at `path`, failing if it exists. The file appears
/// under its final name only once fully written.
fn write_new(path: &Path, data: &[u8], _mode: u32) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("cannot write {}: {e}", path.display());
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    if let Some(dir) = dir {
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(dir).map_err(fail)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = Path::new(&tmp);
    let _ = fs::remove_file(tmp); // left over by an interrupted earlier start
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, _mode);
    let written = opts.open(tmp).and_then(|mut f| {
        f.write_all(data)?;
        f.sync_all()
    });
    let linked = written.and_then(|()| fs::hard_link(tmp, path));
    let _ = fs::remove_file(tmp);
    linked.map_err(fail)?;
    #[cfg(unix)]
    if let Some(dir) = dir {
        let _ = fs::File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
}

fn warn_if_exposed(_key: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = fs::metadata(_key)
            && m.permissions().mode() & 0o077 != 0
        {
            log!(
                "warning: {} is readable or writable by other users; run chmod 600 on it",
                _key.display()
            );
        }
    }
}

/// Accepts connections on all listeners until every accept loop has ended (which
/// should never happen).
pub async fn serve(listeners: Vec<TcpListener>, acceptor: TlsAcceptor, config: Config) {
    let shared = Shared::new(config, acceptor);
    let mut loops = tokio::task::JoinSet::new();
    for listener in listeners {
        loops.spawn(accept_loop(shared.clone(), listener));
    }
    while loops.join_next().await.is_some() {}
}

struct Shared {
    config: Config,
    acceptor: TlsAcceptor,
    /// channel key -> members by client id.
    channels: Mutex<HashMap<String, HashMap<u64, Member>>>,
    conns: Mutex<ConnCount>,
    next_id: AtomicU64,
    /// Randomly seeded per process, so logged channel tags cannot be reversed.
    hasher: RandomState,
    /// Rate limit for lines anyone can trigger without a key: window start, lines
    /// logged in the window, lines suppressed.
    noisy: Mutex<(Instant, u32, u32)>,
}

/// Locks a mutex even if a panicking thread poisoned it: the protected maps stay
/// consistent because no code panics half-way through an update.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct ConnCount {
    total: usize,
    per_ip: HashMap<IpAddr, usize>,
}

/// The address used for per-IP limits: IPv4 (and IPv4-mapped IPv6) addresses as is,
/// other IPv6 addresses by their /64 prefix, which a single host usually controls.
fn ip_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !u128::from(u64::MAX))),
        v4 => v4,
    }
}

/// Releases a connection slot when dropped.
struct ConnGuard {
    shared: Arc<Shared>,
    ip: IpAddr,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let mut c = lock(&self.shared.conns);
        c.total = c.total.saturating_sub(1);
        if let Some(n) = c.per_ip.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                c.per_ip.remove(&self.ip);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Master,
    Slave,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Master => "master",
            Role::Slave => "slave",
        }
    }

    /// For the log: the plain meaning first, the protocol term kept for grep.
    fn label(self) -> &'static str {
        match self {
            Role::Master => "controlling (master)",
            Role::Slave => "controlled (slave)",
        }
    }
}

#[derive(Clone)]
struct Member {
    role: Role,
    protocol_version: u64,
    queue: Arc<Queue>,
}

/// A client's outbound queue, bounded in bytes. Its writer task drains it.
/// Nothing ever waits for room in a queue: a client whose queue would overflow is
/// disconnected, so a stuck client never delays anyone else.
struct Queue {
    tx: mpsc::UnboundedSender<Queued>,
    /// One permit per queued byte; closed when the client is being disconnected.
    budget: Arc<Semaphore>,
    size: usize,
    kill: Notify,
    /// Why the client was disconnected (the first reason wins).
    why: OnceLock<&'static str>,
}

/// A queued line. Dropping it (written, or discarded with the queue) frees its bytes.
struct Queued {
    line: Arc<str>,
    _bytes: OwnedSemaphorePermit,
}

impl Queue {
    fn new(size: usize) -> (Arc<Queue>, mpsc::UnboundedReceiver<Queued>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let q = Queue {
            tx,
            budget: Arc::new(Semaphore::new(size)),
            size,
            kill: Notify::new(),
            why: OnceLock::new(),
        };
        (Arc::new(q), rx)
    }

    /// Disconnects the client (its session sees the notification).
    fn kill(&self, why: &'static str) {
        let _ = self.why.set(why);
        self.budget.close();
        self.kill.notify_one();
    }

    /// Queues a line without waiting, or disconnects the client if its queue is full.
    fn push(&self, line: Arc<str>) {
        let n = permits(&line, self.size);
        match self.budget.clone().try_acquire_many_owned(n) {
            Ok(bytes) => {
                let _ = self.tx.send(Queued {
                    line,
                    _bytes: bytes,
                });
            }
            Err(_) => self.kill("outbound queue full (client too slow to receive)"),
        }
    }
}

/// Permits for a line: its size, capped so a line larger than a budget still fits
/// in an otherwise empty queue.
fn permits(line: &str, cap: usize) -> u32 {
    line.len().min(cap).min(u32::MAX as usize) as u32
}

/// Serializes a server-generated message as one line. Like NVDA's server, protocol
/// version 1 clients do not get the `origin`, `clients` and `client` fields.
fn encode(mut msg: Value, version: u64) -> Arc<str> {
    if version <= 1
        && let Value::Object(m) = &mut msg
    {
        for k in ["origin", "clients", "client"] {
            m.remove(k);
        }
    }
    let mut line = msg.to_string();
    line.push('\n');
    line.into()
}

/// Message types only the server may send, or that make no sense once joined.
const SERVER_ONLY: [&str; 11] = [
    "channel_joined",
    "client_joined",
    "client_left",
    "motd",
    "version_mismatch",
    "nvda_not_connected",
    "error",
    "ping",
    "generate_key",
    "join",
    "protocol_version",
];

/// The type of a client message if it may be relayed: it needs a `type` and no
/// server-only type. Python keeps the last of duplicated keys, so every occurrence
/// is checked.
fn relay_type(fields: &[Field]) -> Option<String> {
    let mut kind = None;
    for f in fields {
        if f.name.as_deref() == Some("type") {
            let t = json::string(f.value).unwrap_or_default();
            if SERVER_ONLY.contains(&t.as_str()) {
                return None;
            }
            kind = Some(t);
        }
    }
    kind
}

impl Shared {
    fn new(config: Config, acceptor: TlsAcceptor) -> Arc<Shared> {
        let config = config.sanitized();
        Arc::new(Shared {
            config,
            acceptor,
            channels: Mutex::default(),
            conns: Mutex::default(),
            next_id: AtomicU64::new(1),
            hasher: RandomState::new(),
            noisy: Mutex::new((Instant::now(), 0, 0)),
        })
    }

    fn admit(self: &Arc<Self>, ip: IpAddr) -> Result<ConnGuard, &'static str> {
        let key = ip_key(ip);
        let mut c = lock(&self.conns);
        let n = c.per_ip.get(&key).copied().unwrap_or(0);
        if c.total >= self.config.max_connections {
            return Err("server connection limit reached");
        }
        if n >= self.config.max_per_ip {
            return Err("per-address connection limit reached");
        }
        c.total += 1;
        c.per_ip.insert(key, n + 1);
        Ok(ConnGuard {
            shared: self.clone(),
            ip: key,
        })
    }

    /// Short non-reversible tag for logging a channel without revealing its key.
    fn tag(&self, channel: &str) -> String {
        format!("{:08x}", self.hasher.hash_one(channel) as u32)
    }

    /// Logs a line anyone can trigger without a key, at most 20 per 10 s.
    fn noisy(&self, msg: std::fmt::Arguments) {
        let mut s = lock(&self.noisy);
        if s.0.elapsed() >= Duration::from_secs(10) {
            if s.2 > 0 {
                log!("({} similar lines suppressed)", s.2);
            }
            *s = (Instant::now(), 0, 0);
        }
        if s.1 < 20 {
            s.1 += 1;
            log!("{msg}");
        } else {
            s.2 += 1;
        }
    }

    /// Adds a member. Its own `channel_joined` is queued under the lock, so it always
    /// precedes any later message about this channel.
    fn join(&self, id: u64, channel: &str, me: Member) {
        let mut channels = lock(&self.channels);
        let members = channels.entry(channel.to_owned()).or_default();
        let clients: Vec<Value> = members
            .iter()
            .map(|(id, m)| json!({"id": id, "connection_type": m.role.name()}))
            .collect();
        let ids: Vec<u64> = members.keys().copied().collect();
        let joined = json!({"type": "channel_joined", "channel": channel, "user_ids": ids, "clients": clients});
        me.queue.push(encode(joined, me.protocol_version));
        if let Some(motd) = &self.config.motd {
            let msg = json!({"type": "motd", "motd": motd});
            me.queue.push(encode(msg, me.protocol_version));
        }
        let note = json!({
            "type": "client_joined", "origin": id, "user_id": id,
            "client": {"id": id, "connection_type": me.role.name()},
        });
        for m in members.values() {
            m.queue.push(encode(note.clone(), m.protocol_version));
        }
        members.insert(id, me);
    }

    fn leave(&self, id: u64, channel: &str) {
        let mut channels = lock(&self.channels);
        let Some(members) = channels.get_mut(channel) else {
            return;
        };
        let Some(me) = members.remove(&id) else {
            return;
        };
        if members.is_empty() {
            channels.remove(channel);
            return;
        }
        let note = json!({
            "type": "client_left", "origin": id, "user_id": id,
            "client": {"id": id, "connection_type": me.role.name()},
        });
        for m in members.values() {
            m.queue.push(encode(note.clone(), m.protocol_version));
        }
    }

    /// Members a message from `id` goes to: those of the other role.
    fn recipients(&self, id: u64, channel: &str, role: Role) -> Vec<Member> {
        let channels = lock(&self.channels);
        let Some(members) = channels.get(channel) else {
            return Vec::new();
        };
        members
            .iter()
            .filter(|&(&other, m)| other != id && m.role != role)
            .map(|(_, m)| m.clone())
            .collect()
    }

    /// Relays a message with `origin` set, encoding it only for the protocol versions
    /// present, outside the lock. Never waits.
    fn forward(&self, me: &Membership<'_>, fields: &[Field<'_>]) {
        let Some(kind) = relay_type(fields) else {
            return;
        };
        let to = self.recipients(me.id, &me.channel, me.member.role);
        if to.is_empty() {
            me.not_connected(&kind);
            return;
        }
        let v2 = to
            .iter()
            .any(|m| m.protocol_version > 1)
            .then(|| json::relay_line(fields, Some(me.id)));
        let v1 = to
            .iter()
            .any(|m| m.protocol_version <= 1)
            .then(|| json::relay_line(fields, None));
        for m in to {
            let line = if m.protocol_version > 1 { &v2 } else { &v1 };
            if let Some(line) = line {
                m.queue.push(line.clone());
            }
        }
    }
}

/// Channel membership of one session; leaving happens on drop, even on a panic.
struct Membership<'a> {
    shared: &'a Shared,
    id: u64,
    channel: String,
    member: Member,
    /// Last `nvda_not_connected` reply, to send at most one per second.
    not_connected_at: std::cell::Cell<Option<Instant>>,
}

impl Membership<'_> {
    /// Tells a controller whose keystroke reached no controlled machine, as NVDA's
    /// previous relays did ("Remote NVDA not connected").
    fn not_connected(&self, kind: &str) {
        if self.member.role != Role::Master || !matches!(kind, "key" | "send_SAS") {
            return;
        }
        let now = Instant::now();
        let last = self.not_connected_at.get();
        if last.is_none_or(|t| now - t >= Duration::from_secs(1)) {
            self.not_connected_at.set(Some(now));
            let msg = json!({"type": "nvda_not_connected"});
            self.member
                .queue
                .push(encode(msg, self.member.protocol_version));
        }
    }
}

impl Drop for Membership<'_> {
    fn drop(&mut self) {
        self.shared.leave(self.id, &self.channel);
        self.member.queue.budget.close();
    }
}

async fn accept_loop(shared: Arc<Shared>, listener: TcpListener) {
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                log!("accept error: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let peer = SocketAddr::new(peer.ip().to_canonical(), peer.port());
        let guard = match shared.admit(peer.ip()) {
            Ok(g) => g,
            Err(reason) => {
                shared.noisy(format_args!("rejected {peer}: {reason}"));
                continue;
            }
        };
        let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
        let shared = shared.clone();
        tokio::spawn(async move {
            let _guard = guard;
            session(&shared, tcp, id, peer).await;
        });
    }
}

/// Keepalive plus, on Linux, TCP_USER_TIMEOUT: a peer that vanished without closing
/// its connection is dropped after about 80 s, even while pings or relayed data
/// are waiting for acknowledgement (which suspends keepalive probes).
fn configure_socket(tcp: &TcpStream) {
    let _ = tcp.set_nodelay(true);
    let sock = SockRef::from(tcp);
    let keepalive = TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(10))
        .with_retries(3);
    let _ = sock.set_tcp_keepalive(&keepalive);
    #[cfg(target_os = "linux")]
    let _ = sock.set_tcp_user_timeout(Some(USER_TIMEOUT));
}

#[cfg(target_os = "linux")]
const USER_TIMEOUT: Duration = Duration::from_secs(75);

/// Aborts a task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Join {
    channel: String,
    role: Role,
    version: u64,
}

enum Hello {
    Join(Join),
    GenerateKey,
}

type Reader = BufReader<tokio::io::ReadHalf<tokio_rustls::server::TlsStream<TcpStream>>>;
type Writer = tokio::io::WriteHalf<tokio_rustls::server::TlsStream<TcpStream>>;

/// Runs one client connection and logs how it ended.
async fn session(shared: &Shared, tcp: TcpStream, id: u64, peer: SocketAddr) {
    let cfg = &shared.config;
    configure_socket(&tcp);
    let mut line = Vec::new();
    let deadline = Instant::now() + cfg.auth_timeout;
    let hello = timeout_at(deadline, async {
        let tls = shared
            .acceptor
            .accept(tcp)
            .await
            .map_err(|e| handshake_failed(&e))?;
        let (rd, wr) = tokio::io::split(tls);
        let mut rd = BufReader::new(rd);
        let hello = hello(&mut rd, cfg.max_line, &mut line).await?;
        Ok::<_, String>((rd, wr, hello))
    })
    .await;
    let (rd, mut wr, hello) = match hello {
        Ok(Ok(x)) => x,
        Ok(Err(outcome)) => {
            return shared.noisy(format_args!("connection {id} from {peer} {outcome}"));
        }
        Err(_) => {
            return shared.noisy(format_args!(
                "connection {id} from {peer} closed: no join before timeout"
            ));
        }
    };
    let join = match hello {
        Hello::Join(join) => join,
        Hello::GenerateKey => {
            let reply = format!(
                "{}\n",
                json!({"type": "generate_key", "key": generate_key()})
            );
            let sent = timeout_at(Instant::now() + cfg.write_timeout, async {
                wr.write_all(reply.as_bytes()).await?;
                wr.shutdown().await
            })
            .await;
            let outcome = match sent {
                Ok(Ok(())) => "key generated",
                _ => "key generated but not delivered",
            };
            return shared.noisy(format_args!("connection {id} from {peer}: {outcome}"));
        }
    };
    let reason = relay(shared, rd, wr, line, id, peer, join).await;
    log!("client {id} left: {reason}");
}

/// Relays a joined client's messages until it disconnects; returns the reason.
async fn relay(
    shared: &Shared,
    mut rd: Reader,
    wr: Writer,
    mut line: Vec<u8>,
    id: u64,
    peer: SocketAddr,
    Join {
        channel,
        role,
        version,
    }: Join,
) -> String {
    let cfg = &shared.config;
    let (queue, rx) = Queue::new(cfg.queue_bytes);
    // The writer runs before the member is published, so its queue always drains,
    // and stops with this task, even on a panic.
    let _writer = AbortOnDrop(tokio::spawn(write_loop(
        wr,
        rx,
        queue.clone(),
        cfg.write_timeout,
        cfg.ping_interval,
    )));
    let member = Member {
        role,
        protocol_version: version,
        queue: queue.clone(),
    };
    shared.join(id, &channel, member.clone());
    let me = Membership {
        shared,
        id,
        channel,
        member,
        not_connected_at: Default::default(),
    };
    log!(
        "client {id} ({peer}) joined channel {} as {}",
        shared.tag(&me.channel),
        role.label()
    );
    let reason = loop {
        tokio::select! {
            r = read_line(&mut rd, cfg.max_line, &mut line, true) => match r {
                Ok(true) => {
                    if line.trim_ascii().is_empty() {
                        continue;
                    }
                    match json::object(&line) {
                        Ok(fields) => shared.forward(&me, &fields),
                        Err(json::Bad::TooManyMembers) => shared.noisy(format_args!(
                            "client {id}: dropped a message with over {} fields", json::MAX_MEMBERS
                        )),
                        Err(json::Bad::Invalid) => break "invalid JSON".to_owned(),
                    }
                }
                Ok(false) => shared.noisy(format_args!("client {id}: dropped a message longer than {} bytes", cfg.max_line)),
                Err(e) => break e.reason(),
            },
            _ = queue.kill.notified() => {
                break queue.why.get().copied().unwrap_or("disconnected by the server").to_owned()
            }
        }
        // Do not keep a large buffer after a rare large message (clipboard).
        if line.capacity() > 64 << 10 {
            line = Vec::new();
        }
    };
    drop(me);
    reason
}

/// Reads messages until a join or key request. An error is the end of a log line
/// starting with "connection N from ADDR".
async fn hello<R: AsyncBufRead + Unpin>(
    rd: &mut R,
    max: usize,
    line: &mut Vec<u8>,
) -> Result<Hello, String> {
    let closed = |reason: &str| format!("closed: {reason}");
    let mut version = 1;
    let mut first = true;
    loop {
        match read_line(rd, max, line, false).await {
            Ok(true) => {}
            Ok(false) => return Err(closed("line too long")),
            // NVDA connects once without verifying the certificate, only to read it
            // and compute its fingerprint.
            Err(end @ (ReadEnd::Closed | ReadEnd::Reset)) if first && line.is_empty() => {
                let reset = match end {
                    ReadEnd::Reset => ", connection reset",
                    _ => "",
                };
                return Err(format!(
                    "closed by the client after the TLS handshake (certificate check{reset})"
                ));
            }
            Err(ReadEnd::Closed) => return Err("closed by the client before joining".into()),
            Err(ReadEnd::Reset) => {
                return Err("closed by the client before joining (connection reset)".into());
            }
            Err(ReadEnd::Failed(e)) => return Err(closed(&e)),
        }
        first = false;
        if line.trim_ascii().is_empty() {
            continue;
        }
        let fields = json::object(line).map_err(|e| match e {
            json::Bad::Invalid => closed("invalid JSON"),
            json::Bad::TooManyMembers => closed("too many fields"),
        })?;
        let get = |key: &str| json::get(&fields, key);
        match get("type").and_then(json::string).as_deref() {
            Some("protocol_version") => {
                version = get("version")
                    .and_then(|v| serde_json::from_str(v).ok())
                    .unwrap_or(version);
            }
            Some("generate_key") => return Ok(Hello::GenerateKey),
            Some("join") => {
                let channel = get("channel").and_then(json::string).unwrap_or_default();
                let role = match get("connection_type").and_then(json::string).as_deref() {
                    Some("master") => Role::Master,
                    Some("slave") => Role::Slave,
                    _ => return Err(closed("invalid join")),
                };
                if channel.is_empty() {
                    return Err(closed("invalid join"));
                }
                if channel.len() > MAX_CHANNEL {
                    return Err(closed("channel name too long"));
                }
                return Ok(Hello::Join(Join {
                    channel,
                    role,
                    version,
                }));
            }
            // Like NVDA's relay, ignore anything else (a reconnecting NVDA may send
            // a message before its join); the auth timeout still applies.
            _ => {}
        }
    }
}

/// Reads one line into `line` without its newline, never buffering more than `max`
/// bytes. Returns false if the line is longer, after reading and discarding it up
/// to its newline if `discard` is set, at once otherwise.
async fn read_line<R: AsyncBufRead + Unpin>(
    rd: &mut R,
    max: usize,
    line: &mut Vec<u8>,
    discard: bool,
) -> Result<bool, ReadEnd> {
    line.clear();
    let mut fits = true;
    loop {
        let buf = rd.fill_buf().await?;
        if buf.is_empty() {
            return Err(ReadEnd::Closed);
        }
        let newline = buf.iter().position(|&b| b == b'\n');
        let chunk = &buf[..newline.unwrap_or(buf.len())];
        if fits && line.len() + chunk.len() > max {
            fits = false;
            line.clear();
            if !discard {
                return Ok(false);
            }
        }
        if fits {
            line.extend_from_slice(chunk);
        }
        let used = chunk.len() + usize::from(newline.is_some());
        rd.consume(used);
        if newline.is_some() {
            return Ok(fits);
        }
    }
}

/// Why reading from a client stopped.
enum ReadEnd {
    /// The client closed the connection, with or without TLS close_notify (NVDA
    /// never sends it).
    Closed,
    /// The client reset the connection, as most stacks do when closing with unread
    /// data, notably Windows clients.
    Reset,
    Failed(String),
}

impl ReadEnd {
    fn reason(self) -> String {
        match self {
            ReadEnd::Closed => "disconnected".into(),
            ReadEnd::Reset => "disconnected (connection reset)".into(),
            ReadEnd::Failed(e) => e,
        }
    }
}

impl From<io::Error> for ReadEnd {
    fn from(e: io::Error) -> Self {
        match (e.kind(), tls_error(&e)) {
            // How rustls reports a missing close_notify (with a docs URL).
            (io::ErrorKind::UnexpectedEof, _) => ReadEnd::Closed,
            (io::ErrorKind::ConnectionReset, _) => ReadEnd::Reset,
            (_, Some(t)) => ReadEnd::Failed(format!("TLS error: {t}")),
            (_, None) => ReadEnd::Failed(format!("read failed: {e}")),
        }
    }
}

fn tls_error(e: &io::Error) -> Option<&rustls::Error> {
    e.get_ref()?.downcast_ref()
}

/// The end of the log line for a failed TLS handshake. With a self-signed certificate,
/// NVDA first tries a verified handshake, which rejects the certificate, and only
/// then checks it against the fingerprint the user accepted.
fn handshake_failed(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::UnexpectedEof => {
            return "closed by the client during the TLS handshake".into();
        }
        io::ErrorKind::ConnectionReset => {
            return "closed by the client during the TLS handshake (connection reset)".into();
        }
        _ => {}
    }
    match tls_error(e) {
        Some(rustls::Error::AlertReceived(
            a @ (AlertDescription::UnknownCA
            | AlertDescription::BadCertificate
            | AlertDescription::CertificateUnknown
            | AlertDescription::UnsupportedCertificate
            | AlertDescription::CertificateExpired),
        )) => format!(
            "closed: the client does not trust the certificate ({a:?} alert; \
             expected from NVDA before it checks the fingerprint)"
        ),
        Some(t) => format!("closed: TLS handshake failed: {t}"),
        None => format!("closed: TLS handshake failed: {e}"),
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut wr: W,
    mut rx: mpsc::UnboundedReceiver<Queued>,
    queue: Arc<Queue>,
    write_timeout: Duration,
    ping_interval: Duration,
) {
    let ping: Arc<str> = "{\"type\":\"ping\"}\n".into();
    let mut tick = tokio::time::interval_at(Instant::now() + ping_interval, ping_interval);
    loop {
        // `_held` keeps the line's bytes counted until it is written.
        let (line, _held) = tokio::select! {
            q = rx.recv() => match q {
                Some(q) => (q.line.clone(), Some(q)),
                None => return,
            },
            _ = tick.tick() => (ping.clone(), None),
        };
        let write = async {
            wr.write_all(line.as_bytes()).await?;
            wr.flush().await
        };
        match timeout(write_timeout, write).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                return queue.kill(match e.kind() {
                    io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe => {
                        "disconnected (connection reset)"
                    }
                    _ => "write failed",
                });
            }
            Err(_) => return queue.kill("write timed out"),
        }
    }
}

/// A random 16-digit key, like NVDA's generated keys but longer.
fn generate_key() -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let rng = SystemRandom::new();
    let mut key = String::new();
    while key.len() < 16 {
        let mut b = [0u8; 16];
        rng.fill(&mut b).expect("system random");
        // Reject values >= 250 to avoid modulo bias.
        key.extend(
            b.iter()
                .filter(|&&x| x < 250)
                .map(|x| char::from(b'0' + x % 10)),
        );
    }
    key.truncate(16);
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(config: Config) -> Arc<Shared> {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("nrps-unit-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (acceptor, _) = load_or_create_tls(&dir.join("c.pem"), &dir.join("k.pem")).unwrap();
        Shared::new(config, acceptor)
    }

    fn member(role: Role) -> (Member, mpsc::UnboundedReceiver<Queued>) {
        let (queue, rx) = Queue::new(1 << 20);
        let m = Member {
            role,
            protocol_version: 2,
            queue,
        };
        (m, rx)
    }

    #[test]
    fn keys_ipv6_by_64_prefix_and_ipv4_by_address() {
        let k = |s: &str| ip_key(s.parse().unwrap());
        assert_eq!(k("2001:db8:1:2:aaaa::1"), k("2001:db8:1:2:ffff:1:2:3"));
        assert_eq!(
            k("2001:db8:1:2::5"),
            "2001:db8:1:2::".parse::<IpAddr>().unwrap()
        );
        assert_ne!(k("2001:db8:1:2::1"), k("2001:db8:1:3::1"));
        assert_eq!(k("::ffff:192.0.2.1"), k("192.0.2.1"));
        assert_ne!(k("192.0.2.1"), k("192.0.2.2"));
        assert_ne!(k("::ffff:192.0.2.1"), k("::ffff:192.0.2.2"));
    }

    #[test]
    fn per_ip_limit_counts_a_64_prefix_together() {
        let s = shared(Config {
            max_per_ip: 2,
            ..Config::default()
        });
        let a = s.admit("2001:db8::1".parse().unwrap()).unwrap();
        let _b = s.admit("2001:db8::ffff:2".parse().unwrap()).unwrap();
        assert!(s.admit("2001:db8::3".parse().unwrap()).is_err());
        assert!(s.admit("2001:db8:0:1::3".parse().unwrap()).is_ok());
        drop(a);
        assert!(s.admit("2001:db8::3".parse().unwrap()).is_ok());
    }

    #[test]
    fn empty_channels_are_removed_and_join_reply_comes_first() {
        let s = shared(Config::default());
        let (a, mut rx_a) = member(Role::Slave);
        let (b, _rx_b) = member(Role::Master);
        s.join(1, "k", a);
        s.join(2, "k", b);
        let first = rx_a.try_recv().unwrap();
        assert!(first.line.starts_with("{\"channel\":\"k\",\"clients\":[]"));
        assert!(rx_a.try_recv().unwrap().line.contains("client_joined"));
        s.leave(2, "k");
        assert!(rx_a.try_recv().unwrap().line.contains("client_left"));
        assert_eq!(lock(&s.channels).len(), 1);
        s.leave(1, "k");
        assert!(lock(&s.channels).is_empty());
    }

    #[test]
    fn a_full_queue_disconnects_only_its_own_client() {
        let (queue, rx) = Queue::new(100);
        let (other, _rx2) = Queue::new(100);
        let line: Arc<str> = "x".repeat(60).into();
        queue.push(line.clone());
        other.push(line.clone());
        assert_eq!(queue.budget.available_permits(), 40);
        queue.push(line.clone());
        assert!(queue.budget.is_closed() && !other.budget.is_closed());
        drop(rx); // the session ends: its queue is freed
        // A line larger than the whole budget still fits in an empty queue.
        let (big, _rx3) = Queue::new(10);
        big.push(line);
        assert!(!big.budget.is_closed());
    }

    #[test]
    fn a_full_queue_records_why() {
        let (queue, _rx) = Queue::new(100);
        let line: Arc<str> = "x".repeat(60).into();
        queue.push(line.clone());
        queue.push(line);
        assert!(queue.why.get().unwrap().starts_with("outbound queue full"));
    }

    #[tokio::test]
    async fn a_stuck_write_records_a_timeout() {
        let (queue, rx) = Queue::new(1 << 20);
        // The peer end is kept open but never read.
        let (wr, _peer) = tokio::io::duplex(8);
        let day = Duration::from_secs(86_400);
        let writer = tokio::spawn(write_loop(
            wr,
            rx,
            queue.clone(),
            Duration::from_millis(50),
            day,
        ));
        queue.push("x".repeat(100).into());
        timeout(Duration::from_secs(5), writer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(queue.why.get().copied(), Some("write timed out"));
    }

    #[test]
    fn config_values_that_would_panic_are_clamped() {
        let c = Config {
            ping_interval: Duration::ZERO,
            auth_timeout: Duration::MAX,
            max_line: 1000,
            queue_bytes: 0,
            ..Config::default()
        }
        .sanitized();
        assert!(c.ping_interval > Duration::ZERO);
        assert!(c.auth_timeout <= Duration::from_secs(86_400));
        assert_eq!(c.queue_bytes, 1000 + QUEUE_MARGIN);
    }

    #[tokio::test]
    async fn accepted_sockets_detect_dead_peers() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = TcpStream::connect(l.local_addr().unwrap()).await.unwrap();
        configure_socket(&c);
        let s = SockRef::from(&c);
        assert!(s.tcp_nodelay().unwrap());
        assert!(s.keepalive().unwrap());
        #[cfg(target_os = "linux")]
        {
            assert_eq!(s.tcp_user_timeout().unwrap(), Some(USER_TIMEOUT));
            assert_eq!(s.tcp_keepalive_time().unwrap(), Duration::from_secs(60));
            assert_eq!(s.tcp_keepalive_retries().unwrap(), 3);
        }
    }

    #[test]
    fn generated_keys_differ() {
        let keys: std::collections::HashSet<String> = (0..100).map(|_| generate_key()).collect();
        assert_eq!(keys.len(), 100);
        assert!(
            keys.iter()
                .all(|k| k.len() == 16 && k.bytes().all(|b| b.is_ascii_digit()))
        );
    }
}
