//! Runs the real binary and checks that its log never contains a key or message
//! contents, whatever clients send.

mod common;

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::process::{Command, Stdio};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use common::{Client, WAIT, temp_dir};
use serde_json::json;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{self, crypto::ring::default_provider, pki_types::ServerName};

const KEY: &str = "SecretChannelKey-7c1e";
const PAYLOAD: &str = "PayloadMarker-93ad";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn logs_never_contain_keys_or_message_contents() {
    let dir = temp_dir();
    let mut child = Command::new(env!("CARGO_BIN_EXE_nrps"))
        .args([
            "--listen",
            "127.0.0.1:0",
            "--max-line",
            "4096",
            "--auth-timeout",
            "2",
        ])
        .arg("--cert")
        .arg(dir.join("cert.pem"))
        .arg("--key")
        .arg(dir.join("key.pem"))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let stderr = child.stderr.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut log = Vec::new();
    let addr: SocketAddr = loop {
        let line = rx.recv_timeout(WAIT).expect("server did not start");
        log.push(line.clone());
        if let Some(a) = line.strip_prefix("listening on ") {
            break a.parse().unwrap();
        }
    };

    // Joins, relayed messages and departures.
    let (mut slave, _) = Client::join(addr, KEY, "slave").await;
    let (mut master, _) = Client::join(addr, KEY, "master").await;
    assert_eq!(slave.recv().await["type"], "client_joined");
    master
        .send(json!({"type": "speak", "sequence": [PAYLOAD], "channel": KEY}))
        .await;
    assert_eq!(slave.recv().await["sequence"][0], PAYLOAD);
    // Oversized and invalid messages after joining.
    let big = format!("{PAYLOAD}{KEY}").repeat(200);
    master
        .send(json!({"type": "speak", "sequence": [big]}))
        .await;
    master
        .send_raw(format!("{{\"type\":\"{PAYLOAD}{KEY}\n").as_bytes())
        .await;
    assert!(master.closed_within(WAIT).await);
    // Nothing left unread, so the drop is a plain close (no close_notify), not a reset.
    assert_eq!(slave.recv().await["type"], "client_left");
    drop(slave);

    // Failed joins carrying the key or payload.
    let long = format!("{KEY}{}", "x".repeat(300));
    for msg in [
        json!({"type": "join", "channel": KEY, "connection_type": PAYLOAD}),
        json!({"type": "join", "channel": long, "connection_type": "slave"}),
        json!({"type": PAYLOAD, "channel": KEY}),
    ] {
        let mut c = Client::connect(addr).await.unwrap();
        c.send(msg).await;
        c.send_raw(format!("{PAYLOAD}{KEY}\n").as_bytes()).await;
        assert!(c.closed_within(WAIT).await);
    }
    let mut c = Client::connect(addr).await.unwrap();
    c.send_raw(format!("{PAYLOAD}{KEY}").repeat(200).as_bytes())
        .await;
    assert!(c.closed_within(WAIT).await);

    // A generated key is a secret too.
    let mut c = Client::connect(addr).await.unwrap();
    c.send(json!({"type": "generate_key"})).await;
    let generated = c.recv().await["key"].as_str().unwrap().to_owned();
    assert!(c.closed_within(WAIT).await);

    // What NVDA does with a self-signed certificate: a verified handshake that
    // rejects it, a connection that only reads it, then (above) a real one. None
    // sends TLS close_notify.
    let roots = rustls::RootCertStore::empty();
    let verified = rustls::ClientConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let name = ServerName::try_from("nrps").unwrap();
    assert!(
        TlsConnector::from(Arc::new(verified))
            .connect(name, tcp)
            .await
            .is_err()
    );
    drop(Client::connect(addr).await.unwrap());
    let mut c = Client::connect(addr).await.unwrap();
    c.send(json!({"type": "protocol_version", "version": 2}))
        .await;
    drop(c);

    // Not TLS at all, and silence until the auth timeout.
    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut tcp, format!("{PAYLOAD}{KEY}\n").as_bytes())
        .await
        .unwrap();
    let _idle = tokio::net::TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    let _ = child.kill();
    let _ = child.wait();
    while let Ok(line) = rx.recv_timeout(Duration::from_secs(2)) {
        log.push(line);
    }
    let log = log.join("\n");
    for secret in [KEY, PAYLOAD, &generated[..], &KEY[..8], &PAYLOAD[..8]] {
        assert!(!log.contains(secret), "{secret:?} logged:\n{log}");
    }
    // Ordinary disconnects read as such, without rustls docs URLs.
    for noise in ["docs.rs", "close_notify", "read error"] {
        assert!(!log.contains(noise), "{noise:?} logged:\n{log}");
    }
    // The events themselves are logged.
    for expected in [
        "joined channel",
        "as controlled (slave)",
        "as controlling (master)",
        "left: invalid JSON",
        "left: disconnected",
        "does not trust the certificate (UnknownCA alert",
        "closed by the client after the TLS handshake (certificate check)",
        "closed by the client before joining",
        "dropped a message longer than 4096 bytes",
        "invalid join",
        "channel name too long",
        "line too long",
        "key generated",
        "no join before timeout",
        "TLS handshake failed",
    ] {
        assert!(log.contains(expected), "{expected:?} missing:\n{log}");
    }
}
