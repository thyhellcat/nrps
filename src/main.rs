use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use socket2::{Domain, Socket, Type};
use tokio::net::TcpListener;

/// Private relay server for NVDA Remote Access.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Address to listen on (repeatable). Default: 0.0.0.0:PORT and [::]:PORT
    /// (see --port). The default IPv6 listener is skipped if IPv6 is unavailable;
    /// any other listener that cannot be opened is fatal. IPv6 listeners accept
    /// IPv6 only.
    #[arg(long)]
    listen: Vec<SocketAddr>,
    /// Port of the default listeners. Cannot be combined with --listen.
    #[arg(long, default_value_t = 6837, conflicts_with = "listen", value_parser = clap::value_parser!(u16).range(1..))]
    port: u16,
    /// TLS certificate (PEM). Generated with the key if both are missing.
    #[arg(long, default_value = "cert.pem")]
    cert: PathBuf,
    /// TLS private key (PEM).
    #[arg(long, default_value = "key.pem")]
    key: PathBuf,
    /// Maximum simultaneous connections. Queued memory is at most this times
    /// --queue-bytes.
    #[arg(long, default_value_t = 32, value_parser = clap::value_parser!(u32).range(1..))]
    max_connections: u32,
    /// Maximum simultaneous connections from one IPv4 address or IPv6 /64 prefix.
    #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u32).range(1..))]
    max_per_ip: u32,
    /// Maximum message line length in bytes. A longer line closes the connection before the join and
    /// is dropped after it.
    #[arg(long, default_value_t = 1 << 20, value_parser = clap::value_parser!(u32).range(1024..))]
    max_line: u32,
    /// Seconds allowed for the TLS handshake plus join.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=86400))]
    auth_timeout: u64,
    /// Seconds allowed for writing one message to a client before it is disconnected.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=86400))]
    write_timeout: u64,
    /// Seconds between keepalive pings.
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..=86400))]
    ping_interval: u64,
    /// Bytes that may wait to be sent to one client; a client that falls further
    /// behind is disconnected. At least --max-line + 16384.
    #[arg(long, default_value_t = 4 << 20, value_parser = clap::value_parser!(u32).range(1024..))]
    queue_bytes: u32,
    /// Message of the day, shown by NVDA in a dialog after joining.
    #[arg(long)]
    motd: Option<String>,
}

fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, None)?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    #[cfg(unix)]
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;
    TcpListener::from_std(socket.into())
}

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = Args::parse();
    let default_listen = args.listen.is_empty();
    if default_listen {
        args.listen = vec![
            SocketAddr::from(([0, 0, 0, 0], args.port)),
            SocketAddr::from(([0u16; 8], args.port)),
        ];
    }
    let min_queue = args.max_line as usize + nrps::QUEUE_MARGIN;
    if (args.queue_bytes as usize) < min_queue {
        eprintln!("--queue-bytes must be at least --max-line + 16384 ({min_queue})");
        return ExitCode::FAILURE;
    }
    let (acceptor, fingerprint) = match nrps::load_or_create_tls(&args.cert, &args.key) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("TLS setup failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("certificate SHA-256 fingerprint: {fingerprint}");

    let mut listeners = Vec::new();
    for addr in &args.listen {
        match bind(*addr) {
            Ok(l) => {
                eprintln!("listening on {}", l.local_addr().unwrap_or(*addr));
                listeners.push(l);
            }
            Err(e) if default_listen && addr.is_ipv6() && !in_use(&e) => {
                eprintln!("IPv6 unavailable, not listening on {addr}: {e}");
            }
            Err(e) => {
                eprintln!("cannot listen on {addr}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let config = nrps::Config {
        max_connections: args.max_connections as usize,
        max_per_ip: args.max_per_ip as usize,
        max_line: args.max_line as usize,
        auth_timeout: Duration::from_secs(args.auth_timeout),
        write_timeout: Duration::from_secs(args.write_timeout),
        ping_interval: Duration::from_secs(args.ping_interval),
        queue_bytes: args.queue_bytes as usize,
        motd: args.motd,
    };
    nrps::serve(listeners, acceptor, config).await;
    eprintln!("all listeners stopped");
    ExitCode::FAILURE
}

fn in_use(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
    )
}
