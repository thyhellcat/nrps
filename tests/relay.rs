//! End-to-end tests: a real server on loopback and fake NVDA clients over TLS, on the
//! multi-threaded runtime the binary uses.

mod common;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use common::{Client, WAIT, temp_dir};
use nrps::{Config, MAX_CHANNEL, load_or_create_tls, serve};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};
use tokio_rustls::rustls::pki_types::CertificateDer;
use tokio_rustls::rustls::{self, ProtocolVersion};

async fn start_on(config: Config, ip: &str) -> SocketAddr {
    let dir = temp_dir();
    let (acceptor, _) = load_or_create_tls(&dir.join("cert.pem"), &dir.join("key.pem")).unwrap();
    let listener = TcpListener::bind((ip, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(vec![listener], acceptor, config));
    addr
}

async fn start(config: Config) -> SocketAddr {
    start_on(config, "127.0.0.1").await
}

/// A connected controller and controlled client in channel `key`; returns
/// (master, slave, master id, slave id).
async fn pair(addr: SocketAddr, key: &str) -> (Client, Client, u64, u64) {
    let (mut slave, _) = Client::join(addr, key, "slave").await;
    let (master, joined) = Client::join(addr, key, "master").await;
    let slave_id = joined["clients"][0]["id"].as_u64().unwrap();
    let note = slave.recv().await;
    assert_eq!(note["type"], "client_joined");
    let master_id = note["user_id"].as_u64().unwrap();
    (master, slave, master_id, slave_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relays_both_ways_with_membership_messages() {
    let addr = start(Config::default()).await;
    let (mut slave, joined) = Client::join(addr, "k1", "slave").await;
    assert_eq!(joined["clients"], json!([]));
    assert_eq!(joined["user_ids"], json!([]));
    assert_eq!(joined["channel"], "k1");

    let (mut master, joined) = Client::join(addr, "k1", "master").await;
    let slave_id = joined["clients"][0]["id"].as_u64().unwrap();
    assert_eq!(joined["clients"][0]["connection_type"], "slave");
    assert_eq!(joined["user_ids"], json!([slave_id]));

    let note = slave.recv().await;
    assert_eq!(note["type"], "client_joined");
    assert_eq!(note["client"]["connection_type"], "master");
    let master_id = note["user_id"].as_u64().unwrap();
    assert_eq!(note["client"]["id"], master_id);
    assert_ne!(master_id, slave_id);

    master
        .send(json!({"type": "key", "vk_code": 65, "pressed": true}))
        .await;
    let m = slave.recv().await;
    assert_eq!(
        m,
        json!({"type": "key", "vk_code": 65, "pressed": true, "origin": master_id})
    );

    // A forged origin is replaced by the real sender id.
    slave
        .send(json!({"type": "speak", "sequence": ["hello"], "origin": 999}))
        .await;
    let m = master.recv().await;
    assert_eq!(
        m,
        json!({"type": "speak", "sequence": ["hello"], "origin": slave_id})
    );

    drop(master);
    let note = slave.recv().await;
    assert_eq!(note["type"], "client_left");
    assert_eq!(note["user_id"], master_id);
    assert_eq!(note["client"]["connection_type"], "master");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relays_nvda_surrogates_and_nan_verbatim() {
    let addr = start(Config::default()).await;
    let (mut master, mut slave, _, slave_id) = pair(addr, "k").await;
    // Exact bytes of NVDA's json.dumps for a lone surrogate, NaN and infinities.
    let line = r#"{"sequence": ["a\ud83d", "\ude00b", "😀"], "rate": NaN, "x": [Infinity, -Infinity, 1000000000000000.0, 0.0015, 1E15], "type": "speak"}"#;
    slave.send_raw(format!("{line}\n").as_bytes()).await;
    let got = master.recv_line().await;
    assert_eq!(
        got,
        format!(
            r#"{{"origin":{slave_id},"sequence":["a\ud83d", "\ude00b", "😀"],"rate":NaN,"x":[Infinity, -Infinity, 1000000000000000.0, 0.0015, 1E15],"type":"speak"}}"#
        ) + "\n"
    );
    // The connection survives and keeps relaying.
    slave.send(json!({"type": "cancel"})).await;
    assert_eq!(master.recv().await["type"], "cancel");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relayed_lines_are_not_expanded() {
    let config = Config {
        max_line: 64 * 1024,
        ..Config::default()
    };
    let addr = start(config).await;
    let (mut master, mut slave, master_id, _) = pair(addr, "k").await;
    let body = vec!["1E15"; 12_000].join(",");
    let line = format!(r#"{{"type":"display","cells":[{body}]}}"#);
    assert!(line.len() < 64 * 1024);
    master.send_raw(format!("{line}\n").as_bytes()).await;
    let got = slave.recv_line().await;
    assert_eq!(
        got,
        format!(r#"{{"origin":{master_id},"type":"display","cells":[{body}]}}"#) + "\n"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn routes_by_role_and_drops_server_only_types() {
    let addr = start(Config::default()).await;
    let (mut m1, mut slave, _, _) = pair(addr, "k").await;
    let (mut m2, _) = Client::join(addr, "k", "master").await;
    assert_eq!(m1.recv().await["type"], "client_joined");
    assert_eq!(slave.recv().await["type"], "client_joined");

    // Controller to controller: not relayed (set_braille_info storms, clipboard).
    m1.send(json!({"type": "set_clipboard_text", "text": "secret"}))
        .await;
    assert_eq!(slave.recv().await["text"], "secret");
    assert!(m2.nothing_within(Duration::from_millis(300)).await);

    // Server-only types and typeless messages are not relayed.
    for msg in [
        json!({"type": "client_left", "user_id": 1}),
        json!({"type": "motd", "motd": "x"}),
        json!({"no_type": 1}),
    ] {
        slave.send(msg).await;
    }
    // Duplicated keys (Python keeps the last one): any server-only type drops it.
    slave
        .send_raw(b"{\"type\":\"speak\",\"type\":\"client_left\"}\n")
        .await;
    slave
        .send_raw(b"{\"type\":\"channel_joined\",\"t\\u0079pe\":\"speak\"}\n")
        .await;
    slave.send(json!({"type": "speak", "n": 1})).await;
    assert_eq!(m1.recv().await["n"], 1);
    assert_eq!(m2.recv().await["n"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tells_a_lone_controller_that_nvda_is_not_connected() {
    let addr = start(Config::default()).await;
    let (mut master, _) = Client::join(addr, "k", "master").await;
    master.send(json!({"type": "key", "vk_code": 65})).await;
    assert_eq!(master.recv().await, json!({"type": "nvda_not_connected"}));
    // At most one per second.
    master.send(json!({"type": "key", "vk_code": 65})).await;
    assert!(master.nothing_within(Duration::from_millis(300)).await);
    // Not for other message types.
    sleep(Duration::from_millis(1100)).await;
    master
        .send(json!({"type": "set_braille_info", "name": "noBraille", "numCells": 0}))
        .await;
    assert!(master.nothing_within(Duration::from_millis(300)).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn protocol_v1_clients_get_no_origin_or_client_fields() {
    let config = Config {
        motd: Some("hello".into()),
        ..Config::default()
    };
    let addr = start(config).await;
    let (mut slave, joined) = Client::join_v(addr, "k", "slave", 1).await;
    assert!(joined.get("clients").is_none());
    assert_eq!(slave.recv().await, json!({"type": "motd", "motd": "hello"}));
    let (mut master, _) = Client::join(addr, "k", "master").await;
    assert_eq!(
        master.recv().await,
        json!({"type": "motd", "motd": "hello"})
    );
    let note = slave.recv().await;
    assert_eq!(note["type"], "client_joined");
    assert!(note.get("client").is_none() && note.get("origin").is_none());
    master
        .send(json!({"type": "key", "origin": 5, "client": 1, "clients": 2, "n": 1}))
        .await;
    assert_eq!(slave.recv().await, json!({"type": "key", "n": 1}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn channels_are_isolated() {
    let addr = start(Config::default()).await;
    let (mut a1, mut b1, _, _) = pair(addr, "one").await;
    let (_, joined) = Client::join(addr, "two", "slave").await;
    assert_eq!(joined["clients"], json!([]));
    assert_eq!(joined["user_ids"], json!([]));
    let (mut a2, mut b2, _, _) = pair(addr, "three").await;

    a1.send(json!({"type": "key", "n": 1})).await;
    a2.send(json!({"type": "key", "n": 2})).await;
    assert_eq!(b1.recv().await["n"], 1);
    assert_eq!(b2.recv().await["n"], 2);
    assert!(b1.nothing_within(Duration::from_millis(300)).await);
    assert!(b2.nothing_within(Duration::from_millis(300)).await);

    // A departure is announced only in its own channel.
    drop(a2);
    assert_eq!(b2.recv().await["type"], "client_left");
    assert!(b1.nothing_within(Duration::from_millis(300)).await);
    assert!(a1.nothing_within(Duration::from_millis(10)).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generates_unpredictable_keys_then_closes() {
    let addr = start(Config::default()).await;
    let mut keys = Vec::new();
    for _ in 0..5 {
        let mut c = Client::connect(addr).await.unwrap();
        c.send(json!({"type": "protocol_version", "version": 2}))
            .await;
        c.send(json!({"type": "generate_key"})).await;
        let m = c.recv().await;
        assert_eq!(m["type"], "generate_key");
        let key = m["key"].as_str().unwrap().to_owned();
        assert_eq!(key.len(), 16);
        assert!(key.bytes().all(|b| b.is_ascii_digit()));
        assert!(key.bytes().any(|b| b != key.as_bytes()[0]), "{key}");
        assert!(c.closed_within(WAIT).await);
        keys.push(key);
    }
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ignores_other_messages_before_join_but_not_garbage() {
    let addr = start(Config::default()).await;
    let mut c = Client::connect(addr).await.unwrap();
    c.send(json!({"type": "key", "vk_code": 65})).await;
    c.send(json!({"no_type": 1})).await;
    c.send_raw(b"\n  \n").await;
    c.send(json!({"type": "join", "channel": "k", "connection_type": "slave"}))
        .await;
    assert_eq!(c.recv().await["type"], "channel_joined");

    let mut c = Client::connect(addr).await.unwrap();
    c.send_raw(b"not json\n").await;
    assert!(c.closed_within(WAIT).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejects_invalid_joins_and_long_channels() {
    let addr = start(Config::default()).await;
    let long = "k".repeat(MAX_CHANNEL + 1);
    for join in [
        json!({"type": "join", "channel": "", "connection_type": "slave"}),
        json!({"type": "join", "channel": "k", "connection_type": "bogus"}),
        json!({"type": "join", "connection_type": "slave"}),
        json!({"type": "join", "channel": 5, "connection_type": "slave"}),
        json!({"type": "join", "channel": long, "connection_type": "slave"}),
    ] {
        let mut c = Client::connect(addr).await.unwrap();
        c.send(join.clone()).await;
        assert!(c.closed_within(WAIT).await, "{join}");
    }
    let longest = "k".repeat(MAX_CHANNEL);
    let (_c, joined) = Client::join(addr, &longest, "slave").await;
    assert_eq!(joined["channel"], longest);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oversized_lines_close_before_join_and_are_dropped_after() {
    let config = Config {
        max_line: 1024,
        auth_timeout: Duration::from_secs(60),
        ..Config::default()
    };
    let addr = start(config).await;

    let mut c = Client::connect(addr).await.unwrap();
    c.send_raw(&[b'a'; 4096]).await;
    assert!(c.closed_within(WAIT).await);

    let (mut master, mut slave, _, _) = pair(addr, "k").await;
    let big = "x".repeat(2000);
    slave
        .send(json!({"type": "speak", "sequence": [big]}))
        .await;
    // Exactly max_line bytes (without the newline) is still accepted.
    let fill = 1024 - r#"{"type":"speak","s":""}"#.len();
    let exact = format!(r#"{{"type":"speak","s":"{}"}}"#, "y".repeat(fill));
    assert_eq!(exact.len(), 1024);
    slave.send_raw(format!("{exact}\n").as_bytes()).await;
    let m = master.recv().await;
    assert_eq!(m["s"].as_str().unwrap().len(), fill);
    let over = format!(r#"{{"type":"speak","s":"{}"}}"#, "y".repeat(fill + 1));
    slave.send_raw(format!("{over}\n").as_bytes()).await;
    slave.send(json!({"type": "speak", "n": 2})).await;
    assert_eq!(master.recv().await["n"], 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closes_idle_and_trickling_unauthenticated_connections() {
    let config = Config {
        auth_timeout: Duration::from_secs(1),
        ..Config::default()
    };
    let addr = start(config).await;

    // Completed TLS handshake but no join.
    let mut c = Client::connect(addr).await.unwrap();
    c.send(json!({"type": "protocol_version", "version": 2}))
        .await;
    assert!(c.closed_within(Duration::from_secs(5)).await);

    // Valid pre-join messages every 300 ms do not extend the deadline.
    let mut c = Client::connect(addr).await.unwrap();
    let begin = Instant::now();
    let closed = timeout(Duration::from_secs(5), async {
        loop {
            c.send(json!({"type": "protocol_version", "version": 2}))
                .await;
            if !c.nothing_within(Duration::from_millis(300)).await {
                return;
            }
        }
    })
    .await;
    assert!(closed.is_ok() && begin.elapsed() < Duration::from_secs(3));

    // Plain TCP that never starts TLS.
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    let mut buf = [0u8; 64];
    let n = timeout(Duration::from_secs(5), tcp.read(&mut buf))
        .await
        .expect("still open after the timeout");
    assert!(matches!(n, Ok(0) | Err(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn joined_clients_may_stay_idle_past_auth_timeout() {
    let config = Config {
        auth_timeout: Duration::from_millis(500),
        ..Config::default()
    };
    let addr = start(config).await;
    let (mut master, mut slave, _, _) = pair(addr, "k").await;
    sleep(Duration::from_secs(2)).await;
    master.send(json!({"type": "key", "n": 1})).await;
    assert_eq!(slave.recv().await["n"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sends_pings() {
    let config = Config {
        ping_interval: Duration::from_millis(200),
        ..Config::default()
    };
    let addr = start(config).await;
    let (mut c, _) = Client::join(addr, "k", "slave").await;
    let mut line = String::new();
    timeout(WAIT, c.rd.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(line, "{\"type\":\"ping\"}\n");
}

/// A connection over the limit is closed before the TLS handshake, while the limit
/// holds, the listener keeps accepting, and slots are released on disconnect.
async fn check_limit(addr: SocketAddr, limit: usize) {
    let mut held = Vec::new();
    for i in 0..limit {
        held.push(Client::join(addr, "k", if i == 0 { "slave" } else { "master" }).await);
    }
    let begin = Instant::now();
    assert!(Client::connect(addr).await.is_err());
    assert!(begin.elapsed() < Duration::from_secs(5)); // not the 60 s auth timeout
    assert!(Client::connect(addr).await.is_err());
    let (_, (mut slave, _)) = (held.pop(), held.remove(0));
    // One slot released (the last master), so a new client gets in.
    let mut joined = None;
    for _ in 0..50 {
        if let Ok(mut c) = Client::connect(addr).await {
            c.send(json!({"type": "join", "channel": "k", "connection_type": "master"}))
                .await;
            joined = Some(c);
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    let mut master = joined.expect("slot not released");
    assert_eq!(master.recv().await["type"], "channel_joined");
    master.send(json!({"type": "key", "n": 1})).await;
    loop {
        let m = slave.recv().await;
        if m["type"] == "key" {
            assert_eq!(m["n"], 1);
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enforces_per_ip_limit() {
    let config = Config {
        max_per_ip: 3,
        auth_timeout: Duration::from_secs(60),
        ..Config::default()
    };
    check_limit(start(config).await, 3).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enforces_per_ip_limit_over_ipv6() {
    let config = Config {
        max_per_ip: 2,
        auth_timeout: Duration::from_secs(60),
        ..Config::default()
    };
    if std::net::TcpListener::bind("[::1]:0").is_err() {
        eprintln!("IPv6 loopback unavailable, skipped");
        return;
    }
    check_limit(start_on(config, "::1").await, 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enforces_global_connection_limit() {
    let config = Config {
        max_connections: 2,
        max_per_ip: 100,
        auth_timeout: Duration::from_secs(60),
        ..Config::default()
    };
    check_limit(start(config).await, 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn negotiates_tls_1_2_like_nvda() {
    let addr = start(Config::default()).await;
    let mut c = Client::connect_with(addr, &[&rustls::version::TLS12])
        .await
        .unwrap();
    assert_eq!(c.tls_version, Some(ProtocolVersion::TLSv1_2));
    c.send(json!({"type": "join", "channel": "k", "connection_type": "slave"}))
        .await;
    assert_eq!(c.recv().await["type"], "channel_joined");
}

/// Reads `count` speak messages numbered from 0, in order.
fn read_all(mut c: Client, count: u64) -> tokio::task::JoinHandle<Client> {
    tokio::spawn(async move {
        let mut n = 0;
        while n < count {
            let m = c.recv().await;
            if m["type"] == "speak" {
                assert_eq!(m["n"], n);
                n += 1;
            }
        }
        c
    })
}

/// Sends `count` numbered speak messages of about 32 KiB, pausing briefly every
/// 256 KiB so a healthy (debug-build) reader can keep within its queue.
fn flood(mut c: Client, count: u64) -> tokio::task::JoinHandle<Client> {
    tokio::spawn(async move {
        let pad = "x".repeat(32 * 1024);
        for n in 0..count {
            c.send(json!({"type": "speak", "n": n, "pad": pad})).await;
            if n % 8 == 7 {
                sleep(Duration::from_millis(1)).await;
            }
        }
        c
    })
}

async fn expect_left(master: &mut Client, id: u64) {
    loop {
        let m = master.recv().await;
        if m["type"] == "client_left" {
            assert_eq!(m["user_id"], id);
            return;
        }
    }
}

/// Small queues, and a write timeout far longer than the tests: any disconnection
/// seen comes from a queue overflowing.
fn small_queues() -> Config {
    Config {
        max_line: 64 * 1024,
        queue_bytes: 1 << 20,
        write_timeout: Duration::from_secs(600),
        ..Config::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stuck_receiver_does_not_delay_a_healthy_one_in_its_channel() {
    let addr = start(small_queues()).await;
    let (_stuck, _) = Client::join(addr, "k", "slave").await;
    let (mut master, joined) = Client::join(addr, "k", "master").await;
    let stuck_id = joined["clients"][0]["id"].as_u64().unwrap();
    let (fast, _) = Client::join(addr, "k", "slave").await;
    assert_eq!(master.recv().await["type"], "client_joined");

    const COUNT: u64 = 800; // 25.6 MiB, far more than socket buffers and queues
    let begin = Instant::now();
    let reader = read_all(fast, COUNT);
    let master = flood(master, COUNT);
    timeout(Duration::from_secs(30), reader)
        .await
        .expect("healthy receiver delayed")
        .unwrap();
    let mut master = timeout(WAIT, master).await.unwrap().unwrap();
    assert!(begin.elapsed() < Duration::from_secs(30));
    expect_left(&mut master, stuck_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stuck_clients_never_disconnect_another_channel() {
    let addr = start(small_queues()).await;
    // Channel "a": four controlled clients that never read, and a flooding controller.
    let mut stuck = Vec::new();
    for _ in 0..4 {
        stuck.push(Client::join(addr, "a", "slave").await.0);
    }
    let (flooder, _) = Client::join(addr, "a", "master").await;
    // Channel "b": the user's controller and controlled PC.
    let (mut master, mut slave, _, _) = pair(addr, "b").await;
    let flooder = flood(flooder, 200);
    let clip = "c".repeat(21 * 1024);
    for n in 0..20 {
        slave
            .send(json!({"type": "set_clipboard_text", "text": clip, "n": n}))
            .await;
        let m = master.recv().await;
        assert_eq!(
            (m["type"].as_str(), m["n"].as_u64()),
            (Some("set_clipboard_text"), Some(n))
        );
        master.send(json!({"type": "key", "n": n})).await;
        assert_eq!(slave.recv().await["n"], n);
        sleep(Duration::from_millis(20)).await;
    }
    timeout(Duration::from_secs(30), flooder)
        .await
        .unwrap()
        .unwrap();
    assert!(master.nothing_within(Duration::from_millis(300)).await);
    assert!(slave.nothing_within(Duration::from_millis(10)).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slower_receiver_survives_a_burst_within_its_queue() {
    let addr = start(Config::default()).await; // 4 MiB queues
    let (master, slave, _, _) = pair(addr, "k").await;
    const COUNT: u64 = 100; // 3.2 MiB
    let reader = tokio::spawn(async move {
        let mut slave = slave;
        for n in 0..COUNT {
            assert_eq!(slave.recv().await["n"], n);
            if n % 10 == 0 {
                sleep(Duration::from_millis(50)).await;
            }
        }
        slave
    });
    let mut master = flood(master, COUNT).await.unwrap();
    let _slave = timeout(Duration::from_secs(30), reader)
        .await
        .expect("receiver stalled")
        .unwrap();
    assert!(master.nothing_within(Duration::from_millis(300)).await); // no client_left
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caps_top_level_fields() {
    let addr = start(small_queues()).await;
    let fields = |n: usize| {
        let m: Vec<String> = (1..n).map(|i| format!("\"k{i}\":{i}")).collect();
        format!("{{\"type\":\"speak\",{}}}\n", m.join(","))
    };
    // Before join: closed.
    let mut c = Client::connect(addr).await.unwrap();
    c.send_raw(fields(65).as_bytes()).await;
    assert!(c.closed_within(WAIT).await);

    let (mut master, mut slave, _, _) = pair(addr, "k").await;
    slave.send_raw(fields(64).as_bytes()).await;
    assert_eq!(master.recv().await["k63"], 63);
    slave.send_raw(fields(65).as_bytes()).await;
    // A max_line message made of tiny members (cheap to parse, costly to hold).
    let tiny = format!("{{\"type\":\"speak\"{}}}\n", ",\"\":0".repeat(12_000));
    assert!(tiny.len() < 64 * 1024);
    for _ in 0..20 {
        slave.send_raw(tiny.as_bytes()).await;
    }
    slave.send(json!({"type": "speak", "n": 2})).await;
    assert_eq!(master.recv().await["n"], 2);
}

#[test]
fn certificate_is_generated_once_and_reused() {
    let dir = temp_dir();
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    let (_, first) = load_or_create_tls(&cert, &key).unwrap();
    let (_, second) = load_or_create_tls(&cert, &key).unwrap();
    assert_eq!(first, second);
    // NVDA shows SHA-256 over the DER certificate.
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    let der = CertificateDer::from_pem_file(&cert).unwrap();
    let digest = ring::digest::digest(&ring::digest::SHA256, &der);
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(first, hex);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert!(!dir.join("key.pem.tmp").exists());
    std::fs::remove_file(&key).unwrap();
    let err = load_or_create_tls(&cert, &key).err().unwrap().to_string();
    assert!(err.contains("only one of"), "{err}");
}

#[test]
fn rejects_same_file_for_cert_and_key() {
    let path = temp_dir().join("both.pem");
    assert!(load_or_create_tls(&path, &path).is_err());
    assert!(!path.exists());
}

#[test]
fn tls_errors_never_echo_key_material() {
    let dir = temp_dir();
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    load_or_create_tls(&cert, &key).unwrap();
    // A key flattened onto one line (e.g. `echo $KEY > key.pem`).
    let flat = std::fs::read_to_string(&key).unwrap().replace('\n', " ");
    std::fs::write(&key, &flat).unwrap();
    let err = load_or_create_tls(&cert, &key).err().unwrap().to_string();
    let body = flat.split_whitespace().nth(4).unwrap();
    assert!(!err.contains(&body[..20]), "{err}");
    assert!(!err.contains("[45, 45"), "{err}");
    assert!(err.contains("key.pem"), "{err}");
}
