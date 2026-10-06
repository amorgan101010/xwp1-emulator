//! The web panel's server: the page and its data over HTTP, and a
//! WebSocket that carries MIDI both ways, status, and (to a page that asks)
//! the sound. The shape follows the other machines of elektremu-studio, so
//! its remote hub can pass the panel through and play it:
//!
//!   GET /          panel/index.html (and the other files of that directory)
//!   GET /ws        WebSocket. The page sends
//!                    binary          MIDI bytes for the instrument
//!                    'a 1' / 'a 0'   stream the sound to this page, or stop
//!                    other text      handed to the player (`control`); its answer, if any, goes back
//!                  and gets
//!                    'A <rate>'      the sample rate of the sound stream
//!                    binary 'A', then 16-bit LE stereo PCM
//!                    binary 'M', then one MIDI message the instrument sent
//!                    binary 'I', then MIDI bytes that reached the instrument
//!                                    from elsewhere (another page, ALSA)
//!                    'S <json>'      status, a few times a second
//!
//! Every connection has its own thread; the emulation thread only ever
//! pushes into bounded queues and never waits for a client. There is no
//! authentication: it is meant for a trusted home network.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

/// What the player does with a text command from a page; Some(text) is sent back.
pub type Control = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
use std::time::Duration;

use tungstenite::Message;

enum Out {
    Text(String),
    Binary(Vec<u8>),
}

struct Client {
    id: usize,
    tx: SyncSender<Out>,
    audio: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct Panel {
    clients: Arc<Mutex<Vec<Client>>>,
    listeners: Arc<AtomicUsize>, // pages that asked for the sound
    epoch: Arc<AtomicUsize>,     // raised to hang up on the pages connected (`hang_up`)
    stopped: Arc<AtomicBool>,
    pub port: u16,
}

impl Panel {
    /// Serve `dir` on `port` (or the next free one of the 20 after it).
    /// MIDI from pages goes to `midi`.
    pub fn start(port: u16, dir: PathBuf, midi: mpsc::Sender<Vec<u8>>, rate: u32, control: Control) -> std::io::Result<Panel> {
        let (listener, port) = (port..port.saturating_add(20))
            .find_map(|p| TcpListener::bind(("0.0.0.0", p)).ok().map(|l| (l, p)))
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrInUse, "no free port"))?;
        let panel = Panel { clients: Default::default(), listeners: Default::default(), epoch: Default::default(),
                            stopped: Default::default(), port };
        let shared = panel.clone();
        std::thread::spawn(move || {
            let mut next_id = 0;
            for stream in listener.incoming().flatten() {
                if shared.stopped.load(Ordering::Relaxed) {
                    break; // the port is free again once the listener is dropped
                }
                let (shared, dir, midi, control) = (shared.clone(), dir.clone(), midi.clone(), control.clone());
                next_id += 1;
                let id = next_id;
                std::thread::spawn(move || serve(stream, id, shared, &dir, midi, rate, control));
            }
        });
        Ok(panel)
    }

    /// Close the connections there are: the pages reconnect and read the instrument again.
    pub fn hang_up(&self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
    }

    /// Stop serving and give the port back (a plugin instance that is removed).
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.hang_up();
        let _ = TcpStream::connect(("127.0.0.1", self.port)); // wakes the listener
    }

    pub fn wants_audio(&self) -> bool {
        self.listeners.load(Ordering::Relaxed) > 0
    }

    /// Interleaved stereo samples, to the pages that asked for them.
    pub fn audio(&self, pcm: &[i16]) {
        let mut frame = Vec::with_capacity(1 + 2 * pcm.len());
        frame.push(b'A');
        frame.extend(pcm.iter().flat_map(|s| s.to_le_bytes()));
        for c in self.clients.lock().unwrap().iter().filter(|c| c.audio.load(Ordering::Relaxed)) {
            let _ = c.tx.try_send(Out::Binary(frame.clone()));
        }
    }

    /// A MIDI message the instrument sent.
    pub fn midi_out(&self, msg: &[u8]) {
        self.binary(b'M', msg, 0);
    }

    /// MIDI that reached the instrument from outside the panel.
    pub fn midi_in(&self, msg: &[u8]) {
        self.binary(b'I', msg, 0);
    }

    pub fn status(&self, json: &str) {
        for c in self.clients.lock().unwrap().iter() {
            let _ = c.tx.try_send(Out::Text(format!("S {json}")));
        }
    }

    fn binary(&self, tag: u8, msg: &[u8], except: usize) {
        let mut frame = Vec::with_capacity(1 + msg.len());
        frame.push(tag);
        frame.extend_from_slice(msg);
        let mut clients = self.clients.lock().unwrap();
        // A page whose queue is full has stopped reading: drop it.
        clients.retain(|c| c.id == except || !matches!(c.tx.try_send(Out::Binary(frame.clone())), Err(TrySendError::Disconnected(_))));
    }
}

fn serve(mut stream: TcpStream, id: usize, panel: Panel, dir: &Path, midi: mpsc::Sender<Vec<u8>>, rate: u32, control: Control) {
    // Look at the request without taking it off the socket (a WebSocket
    // handshake is left for tungstenite to read). A proxy may deliver the
    // head in pieces: wait until it is all there.
    let mut head = [0u8; 4096];
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut n = 0;
    for _ in 0..10 {
        n = stream.peek(&mut head).unwrap_or(0);
        if n == head.len() || head[..n].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = stream.set_read_timeout(None);
    let request = String::from_utf8_lossy(&head[..n]).to_string();
    let path = request.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
    if path == "/ws" && request.to_ascii_lowercase().contains("upgrade: websocket") {
        socket(stream, id, panel, midi, rate, control);
        return;
    }
    // Plain HTTP: read the request off the socket, answer with one file.
    let mut buf = [0u8; 4096];
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut seen = Vec::new();
    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => seen.extend_from_slice(&buf[..n]),
        }
    }
    let name = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
    let safe = !name.split('/').any(|part| part.is_empty() || part.starts_with('.'));
    let generated = matches!(name, "data.json" | "hex.json" | "drawbar.json" | "pcm.json" | "mem.json" | "hex_mem.json" | "draw_mem.json" | "pcm_mem.json" | "perf_mem.json" | "waves.json" | "waves.bin");
    let file = if generated && crate::setup::generated_dir().join(name).is_file() {
        crate::setup::generated_dir().join(name)
    } else { dir.join(name) };
    let (status, kind, body) = match safe.then(|| std::fs::read(file).ok()).flatten() {
        Some(body) => {
            let kind = match name.rsplit('.').next().unwrap_or("") {
                "html" => "text/html; charset=utf-8",
                "js" => "text/javascript; charset=utf-8",
                "css" => "text/css; charset=utf-8",
                "json" => "application/json",
                "svg" => "image/svg+xml",
                "ttf" => "font/ttf",
                "png" => "image/png",
                _ => "application/octet-stream",
            };
            ("200 OK", kind, body)
        }
        None => ("404 Not Found", "text/plain", b"not found".to_vec()),
    };
    let header = format!("HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
                         body.len());
    let _ = stream.write_all(header.as_bytes()).and_then(|_| stream.write_all(&body));
}

fn socket(stream: TcpStream, id: usize, panel: Panel, midi: mpsc::Sender<Vec<u8>>, rate: u32, control: Control) {
    let _ = stream.set_nodelay(true);
    let Ok(mut ws) = tungstenite::accept(stream) else { return };
    let _ = ws.get_ref().set_read_timeout(Some(Duration::from_millis(4)));
    let (tx, rx) = mpsc::sync_channel::<Out>(512);
    let audio = Arc::new(AtomicBool::new(false));
    panel.clients.lock().unwrap().push(Client { id, tx, audio: audio.clone() });
    let set_audio = |on: bool| {
        if audio.swap(on, Ordering::Relaxed) != on {
            if on {
                panel.listeners.fetch_add(1, Ordering::Relaxed);
            } else {
                panel.listeners.fetch_sub(1, Ordering::Relaxed);
            }
        }
    };
    let epoch = panel.epoch.load(Ordering::Relaxed);
    'run: loop {
        if panel.epoch.load(Ordering::Relaxed) != epoch {
            break;
        }
        match ws.read() {
            Ok(Message::Binary(bytes)) => {
                panel.binary(b'I', &bytes, id); // the other pages follow along
                if midi.send(bytes.to_vec()).is_err() {
                    break;
                }
            }
            Ok(Message::Text(text)) => match text.as_str() {
                "a 1" => {
                    if ws.send(Message::text(format!("A {rate}"))).is_err() {
                        break;
                    }
                    set_audio(true);
                }
                "a 0" => set_audio(false),
                other => {
                    if let Some(answer) = control(other) {
                        if ws.send(Message::text(answer)).is_err() {
                            break;
                        }
                    }
                }
            },
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
        while let Ok(out) = rx.try_recv() {
            let message = match out {
                Out::Text(t) => Message::text(t),
                Out::Binary(b) => Message::binary(b),
            };
            if ws.write(message).is_err() {
                break 'run;
            }
        }
        if ws.flush().is_err() {
            break;
        }
    }
    set_audio(false);
    panel.clients.lock().unwrap().retain(|c| c.id != id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    #[test]
    fn serves_panel_files_and_rejects_parent_paths() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "xwp1-panel-test-{}-{}", std::process::id(), NEXT.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("index.html"), b"<title>test panel</title>").unwrap();
        std::fs::write(dir.join("app.js"), b"window.testPanel = true;").unwrap();
        let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (midi, _) = mpsc::channel();
        let panel = Panel::start(port, dir.clone(), midi, 48_000, Arc::new(|_| None)).unwrap();
        let request = |path: &str| {
            let mut stream = TcpStream::connect(("127.0.0.1", panel.port)).unwrap();
            stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).unwrap();
            reply
        };
        let index = request("/");
        assert!(index.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(index.contains("Content-Type: text/html; charset=utf-8"));
        assert!(index.ends_with("<title>test panel</title>"));
        assert!(request("/app.js?version=1").ends_with("window.testPanel = true;"));
        assert!(request("/../index.html").starts_with("HTTP/1.1 404 Not Found\r\n"));
        panel.stop();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
