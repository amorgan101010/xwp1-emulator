use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use tungstenite::{client, Message, WebSocket};
use xwp1::panel::Panel;

const TIMEOUT: Duration = Duration::from_secs(2);

struct Fixture {
    panel: Panel,
    midi: mpsc::Receiver<Vec<u8>>,
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("panel");
        let (midi_tx, midi) = mpsc::channel();
        let panel = Panel::start(
            port,
            dir.clone(),
            midi_tx,
            48_000,
            Arc::new(|command| Some(format!("reply {command}"))),
        )
        .unwrap();
        Self { panel, midi, dir }
    }

    fn socket(&self) -> WebSocket<TcpStream> {
        let stream = TcpStream::connect(("127.0.0.1", self.panel.port)).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        let (mut ws, _) = client(format!("ws://127.0.0.1:{}/ws", self.panel.port), stream).unwrap();
        ws.send(Message::text("ready")).unwrap();
        assert_eq!(ws.read().unwrap().into_text().unwrap(), "reply ready");
        ws
    }

    fn get(&self, path: &str) -> (String, Vec<u8>) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.panel.port)).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let split = response.windows(4).position(|b| b == b"\r\n\r\n").unwrap();
        (
            String::from_utf8(response[..split].to_vec()).unwrap(),
            response[split + 4..].to_vec(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.panel.stop();
    }
}

fn expect_binary(ws: &mut WebSocket<TcpStream>, bytes: &[u8]) {
    let message = ws.read().unwrap();
    assert!(message.is_binary(), "expected binary, got {message:?}");
    assert_eq!(message.into_data(), bytes);
}

fn expect_text(ws: &mut WebSocket<TcpStream>, text: &str) {
    let message = ws.read().unwrap();
    assert!(message.is_text(), "expected text, got {message:?}");
    assert_eq!(message.into_text().unwrap(), text);
}

fn eventually(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !predicate() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        predicate(),
        "condition did not become true within {TIMEOUT:?}"
    );
}

#[test]
fn serves_real_assets_with_correct_headers_and_rejects_bad_paths() {
    let fixture = Fixture::new();
    for (path, file, kind) in [
        ("/", "index.html", "text/html; charset=utf-8"),
        (
            "/stepgrid.js?version=1",
            "stepgrid.js",
            "text/javascript; charset=utf-8",
        ),
        ("/style.css", "style.css", "text/css; charset=utf-8"),
        ("/dsp.json", "dsp.json", "application/json"),
        (
            "/fonts/ChakraPetch-Medium.ttf",
            "fonts/ChakraPetch-Medium.ttf",
            "font/ttf",
        ),
    ] {
        let (header, body) = fixture.get(path);
        assert!(
            header.starts_with("HTTP/1.1 200 OK\r\n"),
            "{path}: {header}"
        );
        assert!(
            header.contains(&format!("Content-Type: {kind}\r\n")),
            "{path}: {header}"
        );
        assert!(header.contains(&format!("Content-Length: {}\r\n", body.len())));
        assert!(header.contains("Cache-Control: no-cache\r\n"));
        assert_eq!(
            body,
            std::fs::read(fixture.dir.join(file)).unwrap(),
            "{path}"
        );
    }
    for path in [
        "/missing.js",
        "/../index.html",
        "/.hidden",
        "/fonts//ChakraPetch-Medium.ttf",
    ] {
        let (header, body) = fixture.get(path);
        assert!(
            header.starts_with("HTTP/1.1 404 Not Found\r\n"),
            "{path}: {header}"
        );
        assert_eq!(body, b"not found", "{path}");
    }
}

#[test]
fn routes_midi_to_engine_and_other_pages_without_echoing_to_sender() {
    let fixture = Fixture::new();
    let mut first = fixture.socket();
    let mut second = fixture.socket();
    let note = [0x90, 60, 100];
    let sysex = [0xf0, 0x7d, 0x58, 0x50, 0xf7];
    for bytes in [&note[..], &sysex[..]] {
        first.send(Message::binary(bytes.to_vec())).unwrap();
        assert_eq!(fixture.midi.recv_timeout(TIMEOUT).unwrap(), bytes);
        let mut frame = vec![b'I'];
        frame.extend_from_slice(bytes);
        expect_binary(&mut second, &frame);
    }
    // A control reply on the same socket exposes an accidental self echo.
    first.send(Message::text("sync")).unwrap();
    expect_text(&mut first, "reply sync");

    fixture.panel.midi_out(&[0x80, 60, 0]);
    expect_binary(&mut first, &[b'M', 0x80, 60, 0]);
    expect_binary(&mut second, &[b'M', 0x80, 60, 0]);
    fixture.panel.midi_in(&[0xb0, 1, 64]);
    expect_binary(&mut first, &[b'I', 0xb0, 1, 64]);
    expect_binary(&mut second, &[b'I', 0xb0, 1, 64]);
    fixture.panel.status("{\"ready\":true}");
    expect_text(&mut first, "S {\"ready\":true}");
    expect_text(&mut second, "S {\"ready\":true}");
}

#[test]
fn audio_reaches_only_subscribers_and_tracks_subscribe_unsubscribe_and_close() {
    let fixture = Fixture::new();
    let mut first = fixture.socket();
    let mut second = fixture.socket();
    assert!(!fixture.panel.wants_audio());

    first.send(Message::text("a 1")).unwrap();
    expect_text(&mut first, "A 48000");
    assert!(fixture.panel.wants_audio());
    first.send(Message::text("a 1")).unwrap();
    expect_text(&mut first, "A 48000");
    second.send(Message::text("a 1")).unwrap();
    expect_text(&mut second, "A 48000");

    let frame = [b'A', 1, 0, 254, 255];
    fixture.panel.audio(&[1, -2]);
    expect_binary(&mut first, &frame);
    expect_binary(&mut second, &frame);

    first.send(Message::text("a 0")).unwrap();
    first.send(Message::text("sync")).unwrap();
    expect_text(&mut first, "reply sync");
    assert!(fixture.panel.wants_audio());
    fixture.panel.audio(&[3, 4]);
    fixture.panel.status("{}");
    expect_text(&mut first, "S {}");
    expect_binary(&mut second, &[b'A', 3, 0, 4, 0]);
    expect_text(&mut second, "S {}");

    second.close(None).unwrap();
    eventually(|| !fixture.panel.wants_audio());
    fixture.panel.audio(&[5, 6]);
    fixture.panel.status("{\"after\":true}");
    expect_text(&mut first, "S {\"after\":true}");
}

#[test]
fn hangup_disconnects_old_pages_and_new_pages_can_reconnect() {
    let fixture = Fixture::new();
    let mut old = fixture.socket();
    old.send(Message::text("a 1")).unwrap();
    expect_text(&mut old, "A 48000");

    fixture.panel.hang_up();
    eventually(|| !fixture.panel.wants_audio());
    assert!(
        old.read().is_err(),
        "old page stayed connected after hangup"
    );

    let mut fresh = fixture.socket();
    fresh.send(Message::binary(vec![0x90, 64, 90])).unwrap();
    assert_eq!(fixture.midi.recv_timeout(TIMEOUT).unwrap(), [0x90, 64, 90]);
    fixture.panel.midi_out(&[0x80, 64, 0]);
    expect_binary(&mut fresh, &[b'M', 0x80, 64, 0]);
}
