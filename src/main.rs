//! herald: desktop notifications.
//!
//! It answers on D-Bus as org.freedesktop.Notifications, the service
//! `notify-send` and every desktop program talk to, and shows each
//! notification as a small box at the top right of the screen. A box
//! closes after its time runs out, or when you click it. Ctrl+Space
//! closes the newest, Ctrl+Shift+Space all of them.
//!
//! Between notifications it sleeps in a blocking read: no timer runs
//! unless a box is on screen.

mod config;
mod paint;

use config::Config;
use paint::Painter;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConnectionExt as _, CreateWindowAux, EventMask, GrabMode, ImageFormat, ModMask, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use zbus::zvariant::OwnedValue;

const PATH: &str = "/org/freedesktop/Notifications";
const IFACE: &str = "org.freedesktop.Notifications";

/// What reaches the main loop, from D-Bus or from the X server.
enum Msg {
    Show(Note),
    Close(u32),
    Click(u32),
    Expose(u32),
    /// Ctrl+Space (false) or Ctrl+Shift+Space (true).
    Key(bool),
}

struct Note {
    id: u32,
    app: String,
    summary: String,
    body: String,
    urgency: u8,
    /// Milliseconds: -1 the urgency's own time, 0 until clicked.
    timeout: i32,
}

/// A box on screen.
struct Popup {
    id: u32,
    win: u32,
    w: u16,
    h: u16,
    pixels: Vec<u8>,
    until: Option<Instant>,
}

/// The D-Bus side: it only hands notes to the main loop.
struct Server {
    tx: Mutex<Sender<Msg>>,
    next: AtomicU32,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<String> {
        vec!["body".into()]
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        _actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id > 0 { replaces_id } else { self.next.fetch_add(1, Ordering::Relaxed) };
        let urgency = hints.get("urgency").and_then(|v| u8::try_from(v).ok()).unwrap_or(1);
        let note = Note { id, app: app_name, summary, body, urgency, timeout: expire_timeout };
        let _ = self.tx.lock().map(|t| t.send(Msg::Show(note)));
        id
    }

    fn close_notification(&self, id: u32) {
        let _ = self.tx.lock().map(|t| t.send(Msg::Close(id)));
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        ("herald".into(), "isene".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into())
    }
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("-v" | "--version") => {
            println!("herald {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("-h" | "--help") => {
            println!("herald — desktop notifications at the top right");
            println!();
            println!("  herald            run (D-Bus also starts it on the first notification)");
            println!("  herald --history  the last notifications, newest last");
            println!();
            println!("  click a box       close it");
            println!("  Ctrl+Space        close the newest; with Shift, all");
            println!("  ~/.heraldrc       colours, size, place, times; `herald --help` lists none of it,");
            println!("                    the README does");
            return;
        }
        Some("--history") => {
            print_history();
            return;
        }
        _ => {}
    }
    let cfg = Config::load();
    if let Err(e) = run(cfg) {
        eprintln!("herald: {e}");
        std::process::exit(1);
    }
}

fn run(cfg: Config) -> Result<(), String> {
    let (conn, screen) = RustConnection::connect(None).map_err(|e| format!("no X display: {e}"))?;
    let conn = Arc::new(conn);
    let scr = &conn.setup().roots[screen];
    let (root, screen_w) = (scr.root, scr.width_in_pixels);

    let (tx, rx) = mpsc::channel::<Msg>();
    let server = Server { tx: Mutex::new(tx.clone()), next: AtomicU32::new(1) };
    let bus = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.name(IFACE))
        .and_then(|b| b.serve_at(PATH, server))
        .and_then(|b| b.build())
        .map_err(|e| format!("cannot take {IFACE} on the session bus (is dunst running?): {e}"))?;

    // The X side: clicks, redraws and the two keys, forwarded as they come.
    {
        let conn = conn.clone();
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            let Ok(ev) = conn.wait_for_event() else { return };
            let msg = match ev {
                Event::ButtonPress(e) => Msg::Click(e.event),
                Event::Expose(e) if e.count == 0 => Msg::Expose(e.window),
                Event::KeyPress(e) => Msg::Key(u16::from(e.state) & u16::from(ModMask::SHIFT) != 0),
                _ => continue,
            };
            if tx.send(msg).is_err() {
                return;
            }
        });
    }

    let painter = Painter::new(&cfg)?;
    let space = keycode_for(&conn, 0x20);
    let mut popups: Vec<Popup> = Vec::new();
    let mut grabbed = false;

    loop {
        // Block until something happens, or until the next box is due.
        let due = popups.iter().filter_map(|p| p.until).min();
        let msg = match due {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(at) => rx.recv_timeout(at.saturating_duration_since(Instant::now())),
        };
        let mut closed: Vec<(u32, u32)> = Vec::new(); // (id, reason)
        match msg {
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
            Ok(Msg::Show(note)) => {
                log_history(&note);
                if cfg.skip.iter().any(|s| *s == note.summary) {
                    continue;
                }
                let (w, h, pixels) = painter.paint(&note);
                let until = match note.timeout {
                    0 => None,
                    t if t > 0 => Some(Instant::now() + Duration::from_millis(t as u64)),
                    _ => cfg.timeout(note.urgency).map(|s| Instant::now() + Duration::from_secs(s)),
                };
                if let Some(p) = popups.iter_mut().find(|p| p.id == note.id) {
                    // A replacement: new text in the same box.
                    p.w = w;
                    p.h = h;
                    p.pixels = pixels;
                    p.until = until;
                    let _ = conn.configure_window(p.win, &x11rb::protocol::xproto::ConfigureWindowAux::new().width(w as u32).height(h as u32));
                    put(&conn, p);
                } else {
                    let win = conn.generate_id().map_err(|e| e.to_string())?;
                    let aux = CreateWindowAux::new()
                        .override_redirect(1)
                        .background_pixel(cfg.colors(note.urgency).1)
                        .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS);
                    let _ = conn.create_window(0, win, root, 0, 0, w, h, 0, WindowClass::INPUT_OUTPUT, 0, &aux);
                    let _ = conn.map_window(win);
                    popups.push(Popup { id: note.id, win, w, h, pixels, until });
                }
            }
            Ok(Msg::Close(id)) => closed.push((id, 3)),
            Ok(Msg::Click(win)) => {
                if let Some(p) = popups.iter().find(|p| p.win == win) {
                    closed.push((p.id, 2));
                }
            }
            Ok(Msg::Expose(win)) => {
                if let Some(p) = popups.iter().find(|p| p.win == win) {
                    put(&conn, p);
                }
            }
            Ok(Msg::Key(all)) => {
                let ids: Vec<u32> = if all { popups.iter().map(|p| p.id).collect() } else { popups.last().map(|p| p.id).into_iter().collect() };
                closed.extend(ids.into_iter().map(|id| (id, 2)));
            }
        }
        let now = Instant::now();
        closed.extend(popups.iter().filter(|p| p.until.is_some_and(|u| u <= now)).map(|p| (p.id, 1)));
        for (id, reason) in closed {
            if let Some(i) = popups.iter().position(|p| p.id == id) {
                let p = popups.remove(i);
                let _ = conn.destroy_window(p.win);
                let _ = bus.emit_signal(None::<&str>, PATH, IFACE, "NotificationClosed", &(id, reason));
            }
        }
        place(&conn, &cfg, &popups, screen_w);
        // Hold the two keys only while a box is up, so Ctrl+Space
        // belongs to other programs the rest of the time.
        if let Some(kc) = space {
            if !popups.is_empty() && !grabbed {
                grab(&conn, root, kc, true);
                grabbed = true;
            } else if popups.is_empty() && grabbed {
                grab(&conn, root, kc, false);
                grabbed = false;
            }
        }
        let _ = conn.flush();
    }
}

/// Stack the boxes down from the top right corner, oldest on top.
fn place(conn: &RustConnection, cfg: &Config, popups: &[Popup], screen_w: u16) {
    let mut y = cfg.y as i32;
    for p in popups {
        let x = screen_w as i32 - p.w as i32 - cfg.x as i32;
        let aux = x11rb::protocol::xproto::ConfigureWindowAux::new()
            .x(x)
            .y(y)
            .stack_mode(x11rb::protocol::xproto::StackMode::ABOVE);
        let _ = conn.configure_window(p.win, &aux);
        y += p.h as i32 + cfg.gap as i32;
    }
}

/// Send a box's pixels, in strips small enough for one X request.
fn put(conn: &RustConnection, p: &Popup) {
    let gc = match conn.generate_id() {
        Ok(g) => g,
        Err(_) => return,
    };
    let _ = conn.create_gc(gc, p.win, &Default::default());
    let row = p.w as usize * 4;
    let rows_per = (200_000 / row.max(1)).max(1);
    let mut y = 0usize;
    while y < p.h as usize {
        let n = rows_per.min(p.h as usize - y);
        let data = &p.pixels[y * row..(y + n) * row];
        let _ = conn.put_image(ImageFormat::Z_PIXMAP, p.win, gc, p.w, n as u16, 0, y as i16, 0, 24, data);
        y += n;
    }
    let _ = conn.free_gc(gc);
}

/// Grab or release Ctrl+Space and Ctrl+Shift+Space, with and without
/// Caps Lock and Num Lock, since a grab matches its modifiers exactly.
fn grab(conn: &RustConnection, root: u32, keycode: u8, on: bool) {
    for extra in [0u16, 2, 16, 18] {
        for shift in [0u16, 1] {
            let mods = ModMask::from(4u16 | extra | shift);
            if on {
                let _ = conn.grab_key(false, root, mods, keycode, GrabMode::ASYNC, GrabMode::ASYNC);
            } else {
                let _ = conn.ungrab_key(keycode, root, mods);
            }
        }
    }
}

/// The key that types this keysym, from the keyboard map.
fn keycode_for(conn: &RustConnection, keysym: u32) -> Option<u8> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn.get_keyboard_mapping(min, max - min + 1).ok()?.reply().ok()?;
    let per = map.keysyms_per_keycode as usize;
    if per == 0 {
        return None;
    }
    map.keysyms.chunks(per).position(|syms| syms.contains(&keysym)).map(|i| min + i as u8)
}

fn history_path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".herald_history")
}

/// One line per notification: time, program, title, text. Kept to the
/// last 200 lines, trimmed once it passes 400.
fn log_history(n: &Note) {
    let clean = |s: &str| paint::plain(s).replace(['\t', '\n'], " ");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let line = format!("{now}\t{}\t{}\t{}\n", clean(&n.app), clean(&n.summary), clean(&n.body));
    let path = history_path();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
    if let Ok(text) = std::fs::read_to_string(&path) {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() > 400 {
            let _ = std::fs::write(&path, lines[lines.len() - 200..].join("\n") + "\n");
        }
    }
}

fn print_history() {
    let text = std::fs::read_to_string(history_path()).unwrap_or_default();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let lines: Vec<&str> = text.lines().collect();
    for l in &lines[lines.len().saturating_sub(15)..] {
        let f: Vec<&str> = l.splitn(4, '\t').collect();
        if f.len() < 4 {
            continue;
        }
        let age = now.saturating_sub(f[0].parse().unwrap_or(now));
        let age = match age {
            0..=59 => format!("{age} s"),
            60..=3599 => format!("{} min", age / 60),
            3600..=86399 => format!("{} h", age / 3600),
            _ => format!("{} d", age / 86400),
        };
        println!("{age:>7} ago  {}: {}  {}", f[1], f[2], f[3]);
    }
}
