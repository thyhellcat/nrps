//! Command-line checks on the real binary.

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use common::temp_dir;

#[test]
fn port_sets_default_listeners() {
    let port = std::net::TcpListener::bind("0.0.0.0:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let dir = temp_dir();
    let mut child = Command::new(env!("CARGO_BIN_EXE_nrps"))
        .args(["--port", &port.to_string()])
        .arg("--cert")
        .arg(dir.join("cert.pem"))
        .arg("--key")
        .arg(dir.join("key.pem"))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let line = loop {
        let line = lines.next().expect("server exited").unwrap();
        if line.starts_with("listening on ") || line.starts_with("cannot listen") {
            break line;
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(line, format!("listening on 0.0.0.0:{port}"));
}

#[test]
fn port_conflicts_with_listen() {
    let out = Command::new(env!("CARGO_BIN_EXE_nrps"))
        .args(["--port", "7000", "--listen", "127.0.0.1:0"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot be used with"), "{err}");
}
