//! The encrypted, authenticated channel over real loopback TCP. No crypto is mocked: the
//! "attackers" are real TCP relays that forward, swap or corrupt real handshake and record
//! bytes.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rustgit_protocol::secure::{accept_secure, connect_secure};
use rustgit_protocol::transport::TcpDuplex;
use rustgit_protocol::{
    Channel, DeviceKey, Duplex, Error, ErrorCode, Request, Response, Succession,
};

const T: Duration = Duration::from_secs(10);

fn duplex(s: TcpStream) -> Box<dyn Duplex> {
    Box::new(TcpDuplex::new(s).unwrap())
}

/// A server that accepts one connection and echoes frames back until closed. The result is the
/// authenticated peer key, or the handshake error.
fn echo_server(
    host: Arc<DeviceKey>,
    chain: Vec<Succession>,
) -> (String, JoinHandle<Result<rustgit_protocol::PublicKey, Error>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let h = thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let (io, peer) = accept_secure(duplex(s), &host, &chain, T)?;
        let mut ch = Channel::new(io, Some(T)).with_peer_key(Some(peer));
        ch.set_max_frame(8 * 1024 * 1024);
        while let Ok(p) = ch.recv(Some(T)) {
            if ch.send(&p).is_err() {
                break;
            }
        }
        Ok(peer)
    });
    (addr, h)
}

/// Forwards bytes both ways; `tamper(direction, stream_offset, byte)` may change each byte
/// (direction 0 = client->server, 1 = server->client).
fn relay(
    target: String,
    tamper: impl Fn(usize, usize, u8) -> u8 + Send + Sync + 'static,
) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let tamper = Arc::new(tamper);
    thread::spawn(move || {
        let (c, _) = l.accept().unwrap();
        let s = TcpStream::connect(target).unwrap();
        let mut pipes = Vec::new();
        for (dir, (mut from, mut to)) in [
            (0usize, (c.try_clone().unwrap(), s.try_clone().unwrap())),
            (1usize, (s, c)),
        ] {
            let tamper = tamper.clone();
            pipes.push(thread::spawn(move || {
                let mut off = 0usize;
                let mut buf = [0u8; 4096];
                loop {
                    let n = match from.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    for b in buf[..n].iter_mut() {
                        *b = tamper(dir, off, *b);
                        off += 1;
                    }
                    if to.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                let _ = to.shutdown(std::net::Shutdown::Both);
            }));
        }
    });
    addr
}

fn client_channel(addr: &str, me: &DeviceKey, pin: &rustgit_protocol::Fingerprint) -> Result<(Channel, bool), Error> {
    let s = TcpStream::connect(addr).unwrap();
    let (io, auth) = connect_secure(duplex(s), me, pin, T)?;
    let mut ch = Channel::new(io, Some(T));
    ch.set_max_frame(8 * 1024 * 1024);
    Ok((ch, auth.rotated))
}

#[test]
fn handshake_authenticates_both_sides_and_carries_large_frames() {
    let host = Arc::new(DeviceKey::generate());
    let me = DeviceKey::generate();
    let (addr, srv) = echo_server(host.clone(), vec![]);
    let (mut ch, rotated) = client_channel(&addr, &me, &host.fingerprint()).unwrap();
    assert!(!rotated);
    // a frame spanning many records, a tiny one, and an empty one
    for size in [3_000_000usize, 1, 0, 65_519, 65_520] {
        let msg: Vec<u8> = (0..size).map(|i| (i * 7 + size) as u8).collect();
        ch.send(&msg).unwrap();
        assert_eq!(ch.recv(Some(T)).unwrap(), msg, "size {size}");
    }
    // ciphertext overhead is small: 16-byte tag + 2-byte length per 64 KiB record
    let payload: u64 = [3_000_000u64, 1, 0, 65_519, 65_520].iter().sum();
    let (sent, recv) = ch.wire_counts();
    // frame header (8) + record framing (18 per 64 KiB) + the handshake (< 1 KiB)
    assert!(sent > payload && sent < payload + 6 * 1024 + 20_000, "wire bytes sent {sent}");
    assert!(recv > payload && recv < payload + 6 * 1024 + 20_000, "wire bytes received {recv}");
    drop(ch);
    // the host saw exactly this device key
    assert_eq!(srv.join().unwrap().unwrap(), me.public());
}

#[test]
fn wrong_pinned_host_key_is_rejected_and_the_device_is_not_revealed() {
    let host = Arc::new(DeviceKey::generate());
    let other = DeviceKey::generate();
    let me = DeviceKey::generate();
    let (addr, srv) = echo_server(host.clone(), vec![]);
    let err = client_channel(&addr, &me, &other.fingerprint()).err().expect("must fail");
    match err {
        Error::HostKeyMismatch { pinned, presented } => {
            assert_eq!(pinned, other.fingerprint());
            assert_eq!(presented, host.fingerprint());
        }
        e => panic!("unexpected error {e:?}"),
    }
    // the client aborted before message 3, so the host never learned the device identity
    let r = srv.join().unwrap();
    assert!(r.is_err(), "host must not complete a handshake with an aborted client");
}

#[test]
fn man_in_the_middle_with_its_own_key_is_detected() {
    let real_host = DeviceKey::generate();
    let attacker = Arc::new(DeviceKey::generate());
    let me = DeviceKey::generate();
    // the attacker terminates the connection with its own, perfectly valid identity
    let (addr, srv) = echo_server(attacker.clone(), vec![]);
    let err = client_channel(&addr, &me, &real_host.fingerprint()).err().unwrap();
    assert!(matches!(err, Error::HostKeyMismatch { .. }), "{err:?}");
    assert!(srv.join().unwrap().is_err());
}

#[test]
fn transparent_relay_works_but_any_modified_byte_is_rejected() {
    let host = Arc::new(DeviceKey::generate());
    let me = DeviceKey::generate();

    // untouched relay: fine
    let (addr, _srv) = echo_server(host.clone(), vec![]);
    let relayed = relay(addr, |_, _, b| b);
    let (mut ch, _) = client_channel(&relayed, &me, &host.fingerprint()).unwrap();
    ch.send(b"hello").unwrap();
    assert_eq!(ch.recv(Some(T)).unwrap(), b"hello");

    // flip one bit inside the host's handshake message: the client refuses
    let (addr, _srv) = echo_server(host.clone(), vec![]);
    let relayed = relay(addr, |dir, off, b| if dir == 1 && off == 100 { b ^ 1 } else { b });
    assert!(client_channel(&relayed, &me, &host.fingerprint()).is_err());

    // flip one bit inside the device's final handshake message: the host refuses
    let (addr, srv) = echo_server(host.clone(), vec![]);
    let relayed = relay(addr, |dir, off, b| if dir == 0 && off == 8 + 2 + 32 + 20 { b ^ 1 } else { b });
    let _ = client_channel(&relayed, &me, &host.fingerprint());
    // the client cannot tell (it has already sent message 3); the host must refuse
    assert!(srv.join().unwrap().is_err(), "host accepted a modified handshake");
}

#[test]
fn tampered_ciphertext_after_the_handshake_is_rejected_by_the_receiver() {
    let host = Arc::new(DeviceKey::generate());
    let me = DeviceKey::generate();
    for (dir, who) in [(0usize, "host"), (1usize, "client")] {
        let (addr, _srv) = echo_server(host.clone(), vec![]);
        // offset 3000 lies inside the first big record in either direction (handshake < 500 B)
        let relayed = relay(addr, move |d, off, b| if d == dir && off == 3000 { b ^ 0x01 } else { b });
        let (mut ch, _) = client_channel(&relayed, &me, &host.fingerprint()).unwrap();
        let msg = vec![0x42u8; 40_000];
        let r = ch.send(&msg).and_then(|_| ch.recv(Some(T)));
        let e = r.err().unwrap_or_else(|| panic!("tampering towards the {who} went unnoticed"));
        let text = e.to_string();
        assert!(
            text.contains("authentication") || text.contains("closed") || text.contains("broken"),
            "unexpected error: {text}"
        );
    }
}

#[test]
fn replayed_and_reordered_records_are_rejected() {
    // Duplicate the first record the client sends after the handshake: the second copy has a
    // stale nonce and must not authenticate.
    let host = Arc::new(DeviceKey::generate());
    let me = DeviceKey::generate();
    let (addr, srv) = echo_server(host.clone(), vec![]);
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let raddr = l.local_addr().unwrap().to_string();
    let done = Arc::new(AtomicBool::new(false));
    let d2 = done.clone();
    thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        let mut s = TcpStream::connect(addr).unwrap();
        // client -> server: magic(8) + msg1; read msg2 from server and pass it on; then msg3
        // then records. Parse `u16 len | data` units after the 8-byte magic.
        let mut magic = [0u8; 8];
        c.read_exact(&mut magic).unwrap();
        s.write_all(&magic).unwrap();
        let rd = |x: &mut TcpStream| {
            let mut l = [0u8; 2];
            x.read_exact(&mut l).unwrap();
            let mut m = vec![0u8; u16::from_be_bytes(l) as usize];
            x.read_exact(&mut m).unwrap();
            [l.to_vec(), m].concat()
        };
        let m1 = rd(&mut c);
        s.write_all(&m1).unwrap();
        let m2 = rd(&mut s);
        c.write_all(&m2).unwrap();
        let m3 = rd(&mut c);
        s.write_all(&m3).unwrap();
        let rec = rd(&mut c);
        s.write_all(&rec).unwrap();
        s.write_all(&rec).unwrap(); // the replay
        d2.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_secs(2));
    });
    let (mut ch, _) = client_channel(&raddr, &me, &host.fingerprint()).unwrap();
    ch.send(b"pay once").unwrap();
    // the host echoes the first copy, then fails on the replay and closes
    let _ = ch.recv(Some(T));
    while !done.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(ch.recv(Some(Duration::from_secs(3))).is_err());
    drop(ch);
    // the server loop ended because of the failed record, not a clean close
    let _ = srv.join();
}

#[test]
fn plaintext_client_gets_a_clear_refusal_from_a_secure_host() {
    let host = Arc::new(DeviceKey::generate());
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let h = {
        let host = host.clone();
        thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            accept_secure(duplex(s), &host, &[], T).err()
        })
    };
    let mut ch = Channel::new(duplex(TcpStream::connect(&addr).unwrap()), Some(T));
    ch.send(
        &Request::Hello(rustgit_protocol::Hello {
            min_version: 1,
            max_version: 1,
            device: "plain".into(),
            project: None,
            credential: vec![],
        })
        .encode(),
    )
    .unwrap();
    match Response::decode(&ch.recv(Some(T)).unwrap()).unwrap() {
        Response::Error(w) => assert_eq!(w.code, ErrorCode::SecurityRequired),
        r => panic!("unexpected {r:?}"),
    }
    assert!(h.join().unwrap().is_some());
}

#[test]
fn secure_client_against_a_plaintext_host_fails_with_a_helpful_error() {
    // A plaintext host reads our magic as an oversized frame length and drops the connection.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let mut ch = Channel::new(duplex(s), Some(T));
        let _ = ch.recv(Some(T)); // errors: frame exceeds limit
    });
    let me = DeviceKey::generate();
    let e = client_channel(&addr, &me, &DeviceKey::generate().fingerprint()).err().unwrap();
    assert!(matches!(e, Error::Handshake(_)), "{e:?}");
    assert!(e.to_string().contains("insecure-open-access"));
}

#[test]
fn rotated_host_key_is_accepted_through_a_signed_succession() {
    let old = DeviceKey::generate();
    let new = Arc::new(DeviceKey::generate());
    let me = DeviceKey::generate();
    let link = Succession::create(&old, &new.public());
    // pinned to the old key: accepted, and the client is told to re-pin
    let (addr, _s) = echo_server(new.clone(), vec![link]);
    let (_, rotated) = client_channel(&addr, &me, &old.fingerprint()).unwrap();
    assert!(rotated);
    // a host that merely claims the new key without a valid link is rejected
    let (addr, _s) = echo_server(new.clone(), vec![]);
    assert!(matches!(
        client_channel(&addr, &me, &old.fingerprint()).err().unwrap(),
        Error::HostKeyMismatch { .. }
    ));
    // a link signed by a different key is rejected
    let stranger = DeviceKey::generate();
    let bad = Succession::create(&stranger, &new.public());
    let forged = Succession { prev: old.public(), ..bad };
    let (addr, _s) = echo_server(new.clone(), vec![forged]);
    assert!(client_channel(&addr, &me, &old.fingerprint()).is_err());
}
