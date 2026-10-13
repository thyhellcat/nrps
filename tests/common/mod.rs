//! Fake NVDA client shared by the integration tests.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::crypto::ring::default_provider;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{
    self, ClientConfig, DigitallySignedStruct, ProtocolVersion, SignatureScheme,
    SupportedProtocolVersion,
};

pub const WAIT: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer,
        _: &[CertificateDer],
        _: &ServerName,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub fn temp_dir() -> std::path::PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nrps-test-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

pub struct Client {
    pub rd: BufReader<ReadHalf<TlsStream<TcpStream>>>,
    pub wr: WriteHalf<TlsStream<TcpStream>>,
    pub tls_version: Option<ProtocolVersion>,
}

impl Client {
    pub async fn connect(addr: SocketAddr) -> std::io::Result<Client> {
        Client::connect_with(addr, rustls::DEFAULT_VERSIONS).await
    }

    pub async fn connect_with(
        addr: SocketAddr,
        versions: &[&'static SupportedProtocolVersion],
    ) -> std::io::Result<Client> {
        let config = ClientConfig::builder_with_provider(Arc::new(default_provider()))
            .with_protocol_versions(versions)
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        let tcp = TcpStream::connect(addr).await?;
        let tls = timeout(
            WAIT,
            TlsConnector::from(Arc::new(config))
                .connect(ServerName::try_from("nrps").unwrap(), tcp),
        )
        .await??;
        let tls_version = tls.get_ref().1.protocol_version();
        let (rd, wr) = tokio::io::split(tls);
        Ok(Client {
            rd: BufReader::new(rd),
            wr,
            tls_version,
        })
    }

    pub async fn join(addr: SocketAddr, key: &str, connection_type: &str) -> (Client, Value) {
        Client::join_v(addr, key, connection_type, 2).await
    }

    pub async fn join_v(
        addr: SocketAddr,
        key: &str,
        connection_type: &str,
        version: u64,
    ) -> (Client, Value) {
        let mut c = Client::connect(addr).await.unwrap();
        c.send(json!({"type": "protocol_version", "version": version}))
            .await;
        c.send(json!({"type": "join", "channel": key, "connection_type": connection_type}))
            .await;
        let joined = c.recv().await;
        assert_eq!(joined["type"], "channel_joined");
        (c, joined)
    }

    pub async fn send(&mut self, msg: Value) {
        self.send_raw(format!("{msg}\n").as_bytes()).await;
    }

    pub async fn send_raw(&mut self, data: &[u8]) {
        let _ = self.wr.write_all(data).await;
        let _ = self.wr.flush().await;
    }

    /// Next line other than ping, as received.
    pub async fn recv_line(&mut self) -> String {
        loop {
            let mut line = String::new();
            let n = timeout(WAIT, self.rd.read_line(&mut line))
                .await
                .expect("recv timeout")
                .unwrap();
            assert!(n > 0, "connection closed");
            if line != "{\"type\":\"ping\"}\n" {
                return line;
            }
        }
    }

    /// Next message other than ping.
    pub async fn recv(&mut self) -> Value {
        serde_json::from_str(&self.recv_line().await).unwrap()
    }

    pub async fn nothing_within(&mut self, d: Duration) -> bool {
        let mut line = String::new();
        timeout(d, self.rd.read_line(&mut line)).await.is_err()
    }

    /// True if the server closes the connection within `d`, ignoring any data.
    pub async fn closed_within(&mut self, d: Duration) -> bool {
        timeout(d, async {
            let mut line = String::new();
            loop {
                line.clear();
                match self.rd.read_line(&mut line).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
            }
        })
        .await
        .is_ok()
    }
}
