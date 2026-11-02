//! herald: desktop notifications.
//!
//! It answers on D-Bus as org.freedesktop.Notifications, the service
//! `notify-send` and every desktop program talk to, and shows each
//! notification as a small box at the top right of the screen. A box
//! closes after its time runs out, or when you click it. Ctrl+Space
//! closes the newest, Ctrl+Shift+Space all of them. `herald --quiet`
//! keeps notifications off the screen for a while, then lists them.
//!
//! Between notifications it sleeps in a blocking read: no timer runs
//! unless a box is on screen.

mod config;
mod paint;
mod view;

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
    /// Start a quiet spell of this many minutes, or end the one running.
    Quiet(u32),
}

/// The id of herald's own boxes, the ones about a quiet spell.
const QUIET_ID: u32 = u32::MAX;
/// Minutes of quiet when `herald --quiet` names none.
const QUIET_MINUTES: u32 = 50;
/// A box lists at most this many of the notifications that waited.
const QUIET_LIST: usize = 6;

struct Note {
    id: u32,
    app: String,
    summary: String,
    body: String,
    urgency: u8,
    /// Milliseconds: -1 the urgency's own time, 0 until clicked.
    timeout: i32,
    /// Hint `category` = `red-alert` (drain sends it on a low battery).
    alert: bool,
}

/// A red alert pulses this many times, then stays lit.
const PULSES: u8 = 8;
const PULSE: Duration = Duration::from_millis(500);

/// A box on screen.
struct Popup {
    id: u32,
    win: u32,
    w: u16,
    h: u16,
    pixels: Vec<u8>,
    until: Option<Instant>,
    /// A red alert's other picture, swapped in at each pulse.
    alt: Option<Vec<u8>>,
    pulses: u8,
    pulse_at: Option<Instant>,
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
        let alert = hints.get("category").and_then(|v| <&str>::try_from(&**v).ok()) == Some("red-alert");
        let note = Note { id, app: app_name, summary, body, urgency, timeout: expire_timeout, alert };
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

/// herald's own commands, beside the standard interface.
struct Control {
    tx: Mutex<Sender<Msg>>,
}

#[zbus::interface(name = "org.isene.Herald")]
impl Control {
    fn quiet(&self, minutes: u32) {
        let _ = self.tx.lock().map(|t| t.send(Msg::Quiet(minutes)));
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
            println!("  herald            in a terminal: the service's state and your notifications");
            println!("                    without one: the service (D-Bus also starts it on demand)");
            println!("  herald --daemon   the service, even from a terminal");
            println!("  herald --history  the last notifications, newest last");
            println!("  herald --quiet [MIN]  no boxes for MIN minutes ({QUIET_MINUTES}), then a list of what came;");
            println!("                    run again to end it early. Critical ones still show");
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
        Some("--quiet") => {
            let minutes = std::env::args().nth(2).and_then(|m| m.parse().ok()).unwrap_or(QUIET_MINUTES);
            let sent = zbus::blocking::Connection::session()
                .and_then(|c| c.call_method(Some(IFACE), PATH, Some("org.isene.Herald"), "Quiet", &minutes).map(|_| ()));
            if let Err(e) = sent {
                eprintln!("herald: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some("--daemon") => {}
        _ => {
            // Inside a terminal (the fe2o3 launcher, a shell) herald shows
            // what it has done; without one it is the service.
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                view::run();
                return;
            }
        }
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
        .and_then(|b| b.serve_at(PATH, Control { tx: Mutex::new(tx.clone()) }))
        .and_then(|b| b.build())
        .map_err(|e| format!("cannot take {IFACE} on the session bus (is dunst running?): {e}"))?;

    // The X side: clicks, redraws and the two keys, forwarded as they come.
    {
        let conn = conn.clone();
        let tx = tx.clone();
        // When the X server goes (frame restarts), herald goes too; the
        // herald the new session starts takes over the bus name.
        std::thread::spawn(move || loop {
            let Ok(ev) = conn.wait_for_event() else { std::process::exit(0) };
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
    // During a quiet spell notifications wait here instead of showing.
    let mut quiet_until: Option<Instant> = None;
    let mut held: Vec<Note> = Vec::new();

    loop {
        // Block until something happens, or until the next box or the end
        // of a quiet spell is due.
        let due = popups.iter().flat_map(|p| [p.until, p.pulse_at]).flatten().chain(quiet_until).min();
        let msg = match due {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(at) => rx.recv_timeout(at.saturating_duration_since(Instant::now())),
        };
        let mut closed: Vec<(u32, u32)> = Vec::new(); // (id, reason)
        match msg {
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
            Ok(Msg::Show(note)) => {
                if note.id != QUIET_ID {
                    log_history(&note);
                }
                if cfg.skip.iter().any(|s| *s == note.summary) {
                    continue;
                }
                if quiet_until.is_some() && note.id != QUIET_ID && note.urgency < 2 && !note.alert {
                    held.retain(|h| h.id != note.id);
                    held.push(note);
                    continue;
                }
                let (w, h, pixels) = painter.paint(&note);
                let alt = note.alert.then(|| painter.paint_alert(&note, false).2);
                let (pulses, pulse_at) = if alt.is_some() { (PULSES, Some(Instant::now() + PULSE)) } else { (0, None) };
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
                    (p.alt, p.pulses, p.pulse_at) = (alt, pulses, pulse_at);
                    let _ = conn.configure_window(p.win, &x11rb::protocol::xproto::ConfigureWindowAux::new().width(w as u32).height(h as u32));
                    put(&conn, p);
                } else {
                    let win = conn.generate_id().map_err(|e| e.to_string())?;
                    let aux = CreateWindowAux::new()
                        .override_redirect(1)
                        .background_pixel(if note.alert { 0 } else { cfg.colors(note.urgency).1 })
                        .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS);
                    let _ = conn.create_window(0, win, root, 0, 0, w, h, 0, WindowClass::INPUT_OUTPUT, 0, &aux);
                    let _ = conn.map_window(win);
                    popups.push(Popup { id: note.id, win, w, h, pixels, until, alt, pulses, pulse_at });
                }
            }
            Ok(Msg::Close(id)) => {
                held.retain(|h| h.id != id);
                closed.push((id, 3));
            }
            Ok(Msg::Quiet(minutes)) => {
                let note = if quiet_until.take().is_some() || minutes == 0 {
                    quiet_over(std::mem::take(&mut held))
                } else {
                    quiet_until = Some(Instant::now() + Duration::from_secs(u64::from(minutes) * 60));
                    quiet_start(minutes)
                };
                let _ = tx.send(Msg::Show(note));
            }
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
        if quiet_until.is_some_and(|q| q <= now) {
            quiet_until = None;
            let _ = tx.send(Msg::Show(quiet_over(std::mem::take(&mut held))));
        }
        closed.extend(popups.iter().filter(|p| p.until.is_some_and(|u| u <= now)).map(|p| (p.id, 1)));
        for (id, reason) in closed {
            if let Some(i) = popups.iter().position(|p| p.id == id) {
                let p = popups.remove(i);
                let _ = conn.destroy_window(p.win);
                let _ = bus.emit_signal(None::<&str>, PATH, IFACE, "NotificationClosed", &(id, reason));
            }
        }
        for p in popups.iter_mut().filter(|p| p.pulse_at.is_some_and(|t| t <= now)) {
            if let Some(alt) = p.alt.as_mut() {
                std::mem::swap(&mut p.pixels, alt);
            }
            put(&conn, p);
            p.pulses -= 1;
            p.pulse_at = (p.pulses > 0).then(|| now + PULSE);
            if p.pulses == 0 {
                p.alt = None;
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

fn quiet_start(minutes: u32) -> Note {
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    let body = format!("Notifications wait {minutes} {unit}, critical ones excepted. The same key ends it.");
    Note { id: QUIET_ID, app: "herald".into(), summary: "Quiet".into(), body, urgency: 0, timeout: -1, alert: false }
}

/// The box at the end of a quiet spell: what came in meanwhile. It stays
/// until clicked when something did.
fn quiet_over(held: Vec<Note>) -> Note {
    let summary = match held.len() {
        0 => "Quiet over: nothing came in".to_string(),
        1 => "Quiet over: 1 came in".to_string(),
        n => format!("Quiet over: {n} came in"),
    };
    let mut lines: Vec<String> = held.iter().take(QUIET_LIST).map(|h| {
        let title = paint::plain(&h.summary);
        if h.app.is_empty() { title } else { format!("{}: {title}", h.app) }
    }).collect();
    if held.len() > QUIET_LIST {
        lines.push("The rest: herald --history".into());
    }
    let timeout = if held.is_empty() { -1 } else { 0 };
    Note { id: QUIET_ID, app: "herald".into(), summary, body: lines.join("\n"), urgency: 1, timeout, alert: false }
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

pub(crate) fn history_path() -> std::path::PathBuf {
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

/// One line of ~/.herald_history.
pub(crate) struct Entry {
    pub time: u64,
    pub app: String,
    pub title: String,
    pub text: String,
}

pub(crate) fn read_history() -> Vec<Entry> {
    let text = std::fs::read_to_string(history_path()).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(4, '\t').collect();
            (f.len() == 4).then(|| Entry { time: f[0].parse().unwrap_or(0), app: f[1].into(), title: f[2].into(), text: f[3].into() })
        })
        .collect()
}

fn print_history() {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let list = read_history();
    for e in &list[list.len().saturating_sub(15)..] {
        println!("{:>7} ago  {}: {}  {}", view::age(now.saturating_sub(e.time)), e.app, e.title, e.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: u32, app: &str, summary: &str) -> Note {
        Note { id, app: app.into(), summary: summary.into(), body: String::new(), urgency: 1, timeout: -1, alert: false }
    }

    #[test]
    fn the_end_of_a_quiet_spell_lists_what_came() {
        let none = quiet_over(Vec::new());
        assert_eq!((none.summary.as_str(), none.timeout), ("Quiet over: nothing came in", -1));
        let two = quiet_over(vec![note(1, "kastrup", "New mail"), note(2, "", "Build done")]);
        assert_eq!((two.summary.as_str(), two.body.as_str(), two.timeout), ("Quiet over: 2 came in", "kastrup: New mail\nBuild done", 0));
        let many = quiet_over((0..9).map(|i| note(i, "a", "t")).collect());
        assert_eq!(many.body.lines().count(), QUIET_LIST + 1);
        assert!(many.body.ends_with("herald --history"));
    }
}
