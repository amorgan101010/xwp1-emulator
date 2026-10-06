//! The XW-P1 emulator as a desktop application: the panel in a window of
//! its own instead of a browser tab.
//!
//!   xwp1-app [PLAYER OPTIONS]   starts the player (`xwp1-rt`, next to this
//!                               binary or $XWP1_RT) on a free port and shows
//!                               its panel; closing the window ends both
//!   xwp1-app xwp1://PORT        shows the panel of a player that is already
//!                               running there: an instance of the plugin
//!                               (its "Open editor" button sends this URL)
//!
//! The firmware and the data built from it are in the user's data root
//! (`xwp1::setup`): the first start asks for Casio's updater ZIP and imports it.

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::unix::WindowExtUnix;
use tao::window::{Icon, WindowBuilder};
use wry::{WebViewBuilder, WebViewBuilderExtUnix};

enum Player {
    Ready(u16),
    Gone(String), // it ended: its last lines
}

fn page(title: &str, text: &str) -> String {
    let text = text.replace('&', "&amp;").replace('<', "&lt;");
    format!("<!doctype html><meta charset=utf-8><style>html{{background:#0f1012;color:#d9b56c;font:15px sans-serif;height:100%}}\
             body{{display:flex;flex-direction:column;align-items:center;justify-content:center;height:100%;margin:0}}\
             h1{{font-size:20px;font-weight:600;letter-spacing:.08em}}pre{{color:#9a938a;font-size:12px;max-width:90%;white-space:pre-wrap}}</style>\
             <h1>{title}</h1><pre>{text}</pre>")
}

fn choose_updater() -> Result<(), String> {
    if xwp1::setup::validate().is_ok() { return Ok(()); }
    let chosen = Command::new("zenity")
        .args(["--file-selection", "--title=Choose XW-P1 updater", "--file-filter=Updater ZIP or image | *.zip *.bin"])
        .output().map_err(|e| format!("Choose an updater ZIP, then run `xwp1 setup FILE`: {e}"))?;
    if !chosen.status.success() { return Err("No updater was chosen. Open XW-P1 again to finish setup.".into()); }
    let source = String::from_utf8_lossy(&chosen.stdout).trim().to_string();
    let mut progress = Command::new("zenity").args(["--progress", "--pulsate", "--auto-close", "--no-cancel",
        "--title=Preparing XW-P1", "--text=Validating firmware and building editor data…"])
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok();
    let result = xwp1::setup::install(std::path::Path::new(&source)).map_err(|e| e.to_string());
    if let Some(mut child) = progress.take() { child.stdin.take(); let _ = child.wait(); }
    result
}

fn start_player(options: &[String], tell: impl Fn(Player) + Send + 'static) -> std::io::Result<Child> {
    let player = std::env::var_os("XWP1_RT").map(PathBuf::from).unwrap_or_else(|| {
        let beside = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("xwp1-rt")));
        beside.filter(|p| p.exists()).unwrap_or_else(|| PathBuf::from("xwp1/target/release/xwp1-rt"))
    });
    let mut cmd = Command::new(&player);
    // any free port from 8810 on: 8800 is left to a player started by hand
    cmd.args(["--remote", "8810"]).args(options).stderr(Stdio::piped());
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", player.display())))?;
    let log = child.stderr.take().unwrap();
    std::thread::spawn(move || {
        let mut last = std::collections::VecDeque::new();
        for line in BufReader::new(log).lines().map_while(Result::ok) {
            eprintln!("{line}");
            if let Some(port) = line.strip_prefix("panel: http://localhost:").and_then(|r| r.trim_end_matches('/').parse().ok()) {
                tell(Player::Ready(port));
            }
            last.push_back(line);
            if last.len() > 12 {
                last.pop_front();
            }
        }
        tell(Player::Gone(Vec::from(last).join("\n")));
    });
    Ok(child)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // xwp1://PORT, as the desktop entry's URL handler passes it (some openers add a slash)
    let attach = args.first().and_then(|a| a.strip_prefix("xwp1://"))
        .filter(|p| p.trim_end_matches('/') != "setup")
        .map(|p| p.trim_end_matches('/').parse::<u16>());
    let setup = if attach.is_none() { choose_updater().err() } else { None };

    let event_loop = EventLoopBuilder::<Player>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title(if attach.is_some() { "XW-P1 (plugin)" } else { "XW-P1" })
        .with_inner_size(LogicalSize::new(1872.0, 1280.0))
        .with_window_icon(Icon::from_rgba(include_bytes!("../assets/xwp1-64.rgba").to_vec(), 64, 64).ok())
        .with_min_inner_size(LogicalSize::new(720.0, 480.0))
        .build(&event_loop)
        .expect("window");

    let child = Arc::new(Mutex::new(None::<Child>));
    let first = match &attach {
        Some(Ok(port)) => Ok(format!("http://localhost:{port}/")),
        Some(Err(_)) => Err(page("Not a panel address", &args[0])),
        None if setup.is_none() => {
            let proxy = event_loop.create_proxy();
            match start_player(&args, move |p| { let _ = proxy.send_event(p); }) {
                Ok(c) => {
                    *child.lock().unwrap() = Some(c);
                    Err(page("XW-P1", "starting the instrument…"))
                }
                Err(e) => Err(page("The player did not start", &e.to_string())),
            }
        }
        None => Err(page("Choose XW-P1 updater", setup.as_deref().unwrap_or("Setup is needed"))),
    };
    let builder = WebViewBuilder::new();
    let builder = match &first {
        Ok(url) => builder.with_url(url),
        Err(html) => builder.with_html(html),
    };
    let webview = builder.build_gtk(window.default_vbox().expect("window box")).expect("web view");

    let mut ready = false;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(Player::Ready(port)) => {
                ready = true;
                let _ = webview.load_url(&format!("http://localhost:{port}/"));
            }
            Event::UserEvent(Player::Gone(log)) => {
                let _ = webview.load_html(&page(if ready { "The player stopped" } else { "The player did not start" }, &log));
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                if let Some(mut c) = child.lock().unwrap().take() {
                    unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
                    let _ = c.wait();
                }
                *flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
