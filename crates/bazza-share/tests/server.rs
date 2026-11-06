use bazza_share::ShareServer;
use bazza_store::{Meeting, NewSegment, Store};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn http_get(port: u16, path: &str, extra: &str) -> (u16, String, Vec<u8>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{extra}\r\n").unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, head, buf[split + 4..].to_vec())
}

#[test]
fn serves_share_page_and_ranged_media() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("bazza.db")).unwrap());
    let m = Meeting::new_recording("Roadmap sync", None);
    store.create_meeting(&m).unwrap();
    store.replace_segments(&m.id, &[NewSegment { start_ms: 0, end_ms: 1000, speaker: Some("Ann".into()), text: "hello <world>".into() }]).unwrap();
    std::fs::write(store.recordings_dir(&m.id).join("mix.wav"), (0..=255u8).collect::<Vec<_>>()).unwrap();
    let share = store.create_share(&m.id, "meeting", None, None).unwrap();

    let server = ShareServer::start(store.clone(), dir.path().join("clips"), 0).unwrap();
    let port = server.port;

    let (status, _, body) = http_get(port, &format!("/s/{}", share.token), "");
    assert_eq!(status, 200);
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("Roadmap sync"));
    assert!(html.contains("hello &lt;world&gt;"));

    let (status, head, body) = http_get(port, &format!("/s/{}/media", share.token), "Range: bytes=10-19\r\n");
    assert_eq!(status, 206, "range requests supported: {head}");
    assert_eq!(body, (10..=19u8).collect::<Vec<_>>());

    let (status, _, _) = http_get(port, "/s/nope", "");
    assert_eq!(status, 404);
    store.set_share_enabled(&share.token, false).unwrap();
    let (status, _, _) = http_get(port, &format!("/s/{}", share.token), "");
    assert_eq!(status, 404, "revoked links stop working");
    server.stop();
}

#[test]
fn stopping_with_an_unread_media_response_is_bounded_and_releases_the_port() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("bazza.db")).unwrap());
    let meeting = Meeting::new_recording("Slow reader", None);
    store.create_meeting(&meeting).unwrap();
    let audio = std::fs::File::create(store.recordings_dir(&meeting.id).join("mix.wav")).unwrap();
    audio.set_len(32 * 1024 * 1024).unwrap();
    drop(audio);
    let share = store.create_share(&meeting.id, "meeting", None, None).unwrap();
    let server = ShareServer::start(store.clone(), dir.path().join("clips"), 0).unwrap();
    let port = server.port;
    let mut reader = TcpStream::connect(("127.0.0.1", port)).unwrap();
    reader.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    write!(reader, "GET /s/{}/media HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n", share.token).unwrap();
    // Confirm an active response, then deliberately leave its large body unread.
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
        assert!(header.len() < 16 * 1024, "bounded HTTP headers");
    }
    assert!(String::from_utf8_lossy(&header).starts_with("HTTP/1.1 200"));
    store.set_share_enabled(&share.token, false).unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let stopper = std::thread::spawn(move || {
        let started = Instant::now();
        server.stop();
        let _ = done_tx.send(started.elapsed());
    });
    let stopped = done_rx.recv_timeout(Duration::from_secs(2));
    // Even an old, unbounded implementation must be unblocked before asserting,
    // so a failed regression cannot leave the test process waiting on this socket.
    drop(reader);
    let elapsed = stopped.expect("shutdown must finish while the client leaves its response unread");
    stopper.join().unwrap();
    assert!(elapsed < Duration::from_secs(2));

    // A completed Stop still supports immediate re-enable on the same port.
    let restarted = ShareServer::start(store, dir.path().join("clips"), port).unwrap();
    let (status, _, _) = http_get(restarted.port, &format!("/s/{}", share.token), "");
    assert_eq!(status, 404, "shutdown/restart must not restore a revoked link");
    restarted.stop();
}
