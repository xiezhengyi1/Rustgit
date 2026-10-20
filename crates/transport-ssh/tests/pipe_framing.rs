//! Framing and robustness of the byte-stream adapter, over real OS pipes (no ssh involved).

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use rustgit_protocol::{Channel, Duplex, Error};
use rustgit_transport_ssh::{PipeDuplex, STDIO_MARKER};

/// Two connected duplexes: what `a` writes `b` reads, and vice versa.
fn pair() -> (PipeDuplex, PipeDuplex) {
    let (r1, w1) = io::pipe().unwrap();
    let (r2, w2) = io::pipe().unwrap();
    (PipeDuplex::new(r2, w1, "a"), PipeDuplex::new(r1, w2, "b"))
}

/// A duplex whose peer is raw pipe ends the test controls: (duplex, bytes-to-duplex, bytes-from-duplex).
fn raw() -> (PipeDuplex, io::PipeWriter, io::PipeReader) {
    let (to_d_r, to_d_w) = io::pipe().unwrap();
    let (from_d_r, from_d_w) = io::pipe().unwrap();
    (PipeDuplex::new(to_d_r, from_d_w, "raw"), to_d_w, from_d_r)
}

#[test]
fn frames_roundtrip_in_both_directions() {
    let (a, b) = pair();
    let mut ca = Channel::new(Box::new(a), Some(Duration::from_secs(5)));
    let mut cb = Channel::new(Box::new(b), Some(Duration::from_secs(5)));
    ca.send(b"hello").unwrap();
    assert_eq!(cb.recv(Some(Duration::from_secs(5))).unwrap(), b"hello");
    cb.send(b"").unwrap();
    assert_eq!(ca.recv(Some(Duration::from_secs(5))).unwrap(), b"");
}

#[test]
fn a_large_frame_survives_bounded_queues() {
    // 12 MiB is far more than the pipe buffer plus both adapter queues, so this only works
    // if reader and writer threads really apply backpressure instead of buffering.
    let (a, b) = pair();
    let mut ca = Channel::new(Box::new(a), Some(Duration::from_secs(20)));
    let mut cb = Channel::new(Box::new(b), Some(Duration::from_secs(20)));
    ca.set_max_frame(16 * 1024 * 1024);
    cb.set_max_frame(16 * 1024 * 1024);
    let payload: Vec<u8> = (0..12 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let expect = payload.clone();
    let t = std::thread::spawn(move || {
        ca.send(&payload).unwrap();
        ca
    });
    let got = cb.recv(Some(Duration::from_secs(20))).unwrap();
    assert_eq!(got.len(), expect.len());
    assert!(got == expect);
    t.join().unwrap();
}

#[test]
fn many_small_frames_keep_order() {
    let (a, b) = pair();
    let mut ca = Channel::new(Box::new(a), Some(Duration::from_secs(5)));
    let mut cb = Channel::new(Box::new(b), Some(Duration::from_secs(5)));
    let t = std::thread::spawn(move || {
        for i in 0..2000u32 {
            ca.send(&i.to_be_bytes()).unwrap();
        }
        ca
    });
    for i in 0..2000u32 {
        assert_eq!(cb.recv(Some(Duration::from_secs(5))).unwrap(), i.to_be_bytes());
    }
    t.join().unwrap();
}

#[test]
fn an_idle_peer_times_out_instead_of_hanging() {
    let (a, _b) = pair();
    let mut ca = Channel::new(Box::new(a), Some(Duration::from_millis(200)));
    let start = Instant::now();
    let e = ca.recv(Some(Duration::from_millis(150))).unwrap_err();
    assert!(matches!(e, Error::Timeout), "{e:?}");
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_frame_that_stalls_midway_times_out() {
    let (d, mut feed, _out) = raw();
    let mut ch = Channel::new(Box::new(d), Some(Duration::from_millis(200)));
    // header announcing 100 bytes, then only 10 of them
    let mut f = Vec::new();
    f.extend_from_slice(&100u32.to_be_bytes());
    f.extend_from_slice(&0u32.to_be_bytes());
    f.extend_from_slice(&[1u8; 10]);
    feed.write_all(&f).unwrap();
    let start = Instant::now();
    let e = ch.recv(Some(Duration::from_secs(5))).unwrap_err();
    assert!(matches!(e, Error::Timeout), "{e:?}");
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_peer_that_closes_is_reported_not_waited_for() {
    let (a, b) = pair();
    let mut ca = Channel::new(Box::new(a), Some(Duration::from_secs(5)));
    drop(b);
    let start = Instant::now();
    let e = ca.recv(Some(Duration::from_secs(5))).unwrap_err();
    assert!(matches!(e, Error::Io(ref io) if io.kind() == io::ErrorKind::UnexpectedEof), "{e:?}");
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_truncated_frame_followed_by_eof_is_an_error() {
    let (d, mut feed, _out) = raw();
    let mut ch = Channel::new(Box::new(d), Some(Duration::from_secs(5)));
    let mut f = Vec::new();
    f.extend_from_slice(&50u32.to_be_bytes());
    f.extend_from_slice(&0u32.to_be_bytes());
    f.extend_from_slice(&[9u8; 20]);
    feed.write_all(&f).unwrap();
    drop(feed);
    assert!(ch.recv(Some(Duration::from_secs(5))).is_err());
}

#[test]
fn corrupt_oversized_and_garbage_frames_are_refused() {
    // bad CRC
    let (d, mut feed, _out) = raw();
    let mut ch = Channel::new(Box::new(d), Some(Duration::from_secs(5)));
    let payload = b"payload";
    let mut f = Vec::new();
    f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    f.extend_from_slice(&(crc32fast_hash(payload) ^ 1).to_be_bytes());
    f.extend_from_slice(payload);
    feed.write_all(&f).unwrap();
    let e = ch.recv(Some(Duration::from_secs(5))).unwrap_err();
    assert!(e.to_string().contains("checksum"), "{e}");

    // length beyond the negotiated maximum is refused before allocating
    let (d, mut feed, _out) = raw();
    let mut ch = Channel::new(Box::new(d), Some(Duration::from_secs(5)));
    let mut f = Vec::new();
    f.extend_from_slice(&u32::MAX.to_be_bytes());
    f.extend_from_slice(&0u32.to_be_bytes());
    feed.write_all(&f).unwrap();
    let e = ch.recv(Some(Duration::from_secs(5))).unwrap_err();
    assert!(e.to_string().contains("exceeds limit"), "{e}");

    // text where a frame should be (what a login banner on the stream would look like)
    let (d, mut feed, _out) = raw();
    let mut ch = Channel::new(Box::new(d), Some(Duration::from_secs(5)));
    feed.write_all(b"Welcome to Ubuntu 24.04 LTS\n").unwrap();
    assert!(ch.recv(Some(Duration::from_secs(5))).is_err());
}

fn crc32fast_hash(b: &[u8]) -> u32 {
    // same CRC-32 (IEEE) the framing uses; implemented here to keep the test independent
    let mut table = [0u32; 256];
    for i in 0..256u32 {
        let mut c = i;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        table[i as usize] = c;
    }
    let mut c = !0u32;
    for &x in b {
        c = table[((c ^ x as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

#[test]
fn a_peer_that_never_reads_makes_writes_time_out() {
    let (d, _feed, _out_never_read) = raw();
    let mut ch = Channel::new(Box::new(d), Some(Duration::from_millis(300)));
    ch.set_max_frame(16 * 1024 * 1024);
    let big = vec![0u8; 8 * 1024 * 1024];
    let start = Instant::now();
    let e = ch.send(&big).unwrap_err();
    assert!(matches!(e, Error::Timeout), "{e:?}");
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[test]
fn writing_to_a_closed_peer_fails_with_broken_pipe() {
    let (to_d_r, _to_d_w) = io::pipe().unwrap();
    let (from_d_r, from_d_w) = io::pipe().unwrap();
    let mut d = PipeDuplex::new(to_d_r, from_d_w, "x");
    drop(from_d_r);
    d.set_timeouts(None, Some(Duration::from_secs(5))).unwrap();
    let mut res = Ok(());
    for _ in 0..50 {
        res = d.write_all(&[0u8; 1024]).and_then(|_| d.flush());
        if res.is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let e = res.unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::BrokenPipe, "{e}");
}

#[test]
fn marker_skips_banner_noise_and_keeps_following_bytes() {
    let (mut d, mut feed, _out) = raw();
    feed.write_all(b"Last login: Mon Oct 19\nWelcome!\n\nRGIT-STDIO/1\nFRAME").unwrap();
    let noise = d
        .sync_to_marker(STDIO_MARKER, 64 * 1024, Duration::from_secs(5))
        .unwrap();
    assert_eq!(noise, b"Last login: Mon Oct 19\nWelcome!\n\n");
    d.set_timeouts(Some(Duration::from_secs(1)), None).unwrap();
    let mut rest = [0u8; 5];
    d.read_exact(&mut rest).unwrap();
    assert_eq!(&rest, b"FRAME");
}

#[test]
fn marker_split_across_reads_and_with_crlf() {
    let (mut d, mut feed, _out) = raw();
    let t = std::thread::spawn(move || {
        feed.write_all(b"noise\r\nRGIT-ST").unwrap();
        feed.flush().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        feed.write_all(b"DIO/1\r\nX").unwrap();
        feed
    });
    d.sync_to_marker(STDIO_MARKER, 64 * 1024, Duration::from_secs(5)).unwrap();
    let mut b = [0u8; 1];
    d.set_timeouts(Some(Duration::from_secs(1)), None).unwrap();
    d.read_exact(&mut b).unwrap();
    assert_eq!(&b, b"X");
    t.join().unwrap();
}

#[test]
fn marker_failures_are_bounded() {
    // never appears: noise limit
    let (mut d, mut feed, _out) = raw();
    feed.write_all(&vec![b'x'; 100_000]).unwrap();
    let e = d.sync_to_marker(STDIO_MARKER, 4096, Duration::from_secs(5)).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");

    // stream ends first
    let (mut d, mut feed, _out) = raw();
    feed.write_all(b"Permission denied\n").unwrap();
    drop(feed);
    let e = d.sync_to_marker(STDIO_MARKER, 4096, Duration::from_secs(5)).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "{e}");

    // silence: timeout
    let (mut d, _feed, _out) = raw();
    let start = Instant::now();
    let e = d.sync_to_marker(STDIO_MARKER, 4096, Duration::from_millis(200)).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}");
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn server_side_announces_itself_with_the_marker() {
    use rustgit_transport_ssh::session_over;
    let (to_s_r, _to_s_w) = io::pipe().unwrap();
    let (from_s_r, from_s_w) = io::pipe().unwrap();
    let _server = session_over(to_s_r, from_s_w, "stdio").unwrap();
    let mut got = Vec::new();
    let mut r = from_s_r;
    let mut b = [0u8; 1];
    while !got.ends_with(b"RGIT-STDIO/1\n") {
        r.read_exact(&mut b).unwrap();
        got.push(b[0]);
    }
    assert_eq!(got, b"\nRGIT-STDIO/1\n");
}
