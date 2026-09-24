//! What herald shows when it starts inside a terminal, as the fe2o3
//! launcher starts it: the service's state and the notifications it
//! has shown, newest first.

use crate::{read_history, Entry};
use crust::{seq, style, Crust, Cursor, Input};
use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RUST_RGB: (u8, u8, u8) = (247, 76, 0);
const TEXT_RGB: (u8, u8, u8) = (225, 225, 230);
const DIM_RGB: (u8, u8, u8) = (140, 140, 150);
const LIVE_RGB: (u8, u8, u8) = (120, 230, 140);
const BAR_BG: (u8, u8, u8) = (38, 38, 38);

pub fn run() {
    Crust::init();
    Crust::set_app_identity("herald");
    let mut top = 0usize;
    let mut note = String::new();
    loop {
        let list = read_history();
        draw(&list, top, &note);
        note.clear();
        let Some(key) = Input::getchr_ms(600_000) else { continue };
        let (_, rows) = Crust::terminal_size();
        let page = (rows as usize).saturating_sub(4).max(1);
        match key.as_str() {
            "q" | "ESC" => break,
            "j" | "DOWN" => top = (top + 1).min(list.len().saturating_sub(1)),
            "k" | "UP" => top = top.saturating_sub(1),
            "PgDOWN" | " " => top = (top + page).min(list.len().saturating_sub(1)),
            "PgUP" => top = top.saturating_sub(page),
            "g" | "HOME" => top = 0,
            "t" => {
                note = match send_test() {
                    Ok(()) => "test sent; it shows at the top right".into(),
                    Err(e) => format!("no test: {e}"),
                };
                // Give the service a moment to write it to the history.
                std::thread::sleep(Duration::from_millis(300));
                top = 0;
            }
            "c" => {
                draw(&list, top, "clear the history? y/n");
                if Input::getchr_ms(60_000).as_deref() == Some("y") {
                    let _ = std::fs::write(crate::history_path(), "");
                    note = "history cleared".into();
                    top = 0;
                }
            }
            _ => {}
        }
    }
    Crust::cleanup();
}

fn draw(list: &[Entry], top: usize, note: &str) {
    let (cols, rows) = Crust::terminal_size();
    let w = cols as usize;
    let mut out = String::new();

    // The bar across the top: is the service running, and what it holds.
    let state = match service() {
        Some((pid, kb)) => style::rgb(&format!("running · pid {pid} · {:.1} MB", kb as f64 / 1024.0), Some(LIVE_RGB), Some(BAR_BG), ""),
        None => style::rgb("not running · D-Bus starts it on the next notification", Some(DIM_RGB), Some(BAR_BG), ""),
    };
    let head = format!(" herald   {}", crust::strip_ansi(&state));
    out.push_str(&format!(
        "{}{}{}{}{}",
        Cursor::at(1, 1),
        style::rgb(" ", None, Some(BAR_BG), ""),
        style::rgb("herald", Some(RUST_RGB), Some(BAR_BG), "b"),
        style::rgb("   ", None, Some(BAR_BG), ""),
        state
    ));
    out.push_str(&style::rgb(&" ".repeat(w.saturating_sub(crust::display_width(&head))), None, Some(BAR_BG), ""));

    // The notifications, newest first.
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let body_rows = (rows as usize).saturating_sub(4);
    for r in 0..body_rows {
        let y = 3 + r as u16;
        let line = match list.iter().rev().nth(top + r) {
            Some(e) => {
                let left = format!(" {:>7}  {:<14} ", age(now.saturating_sub(e.time)), take(&e.app, 14));
                let title = take(&e.title, w.saturating_sub(crust::display_width(&left) + 1));
                let room = w.saturating_sub(crust::display_width(&left) + crust::display_width(&title) + 3);
                format!(
                    "{}{}  {}",
                    style::rgb(&left, Some(DIM_RGB), None, ""),
                    style::rgb(&title, Some(TEXT_RGB), None, "b"),
                    style::rgb(&take(&e.text, room), Some(DIM_RGB), None, "")
                )
            }
            None if r == 0 && list.is_empty() => style::rgb("   no notifications yet; t sends a test", Some(DIM_RGB), None, "i"),
            None => String::new(),
        };
        out.push_str(&format!("{}{}{}", Cursor::at(1, y), line, seq::ERASE_EOL));
    }

    // The bar along the bottom, the version at the far right.
    let foot = if note.is_empty() { "t test · c clear · j k move · q quit".to_string() } else { note.to_string() };
    let version = format!("v{} ", env!("CARGO_PKG_VERSION"));
    let foot = format!(" {}", take(&foot, w.saturating_sub(version.len() + 3)));
    let pad = w.saturating_sub(crust::display_width(&foot) + version.len());
    out.push_str(&format!(
        "{}{}{}",
        Cursor::at(1, rows),
        style::rgb(&format!("{foot}{}", " ".repeat(pad)), Some((200, 200, 205)), Some(BAR_BG), ""),
        style::rgb(&version, Some(DIM_RGB), Some(BAR_BG), "")
    ));
    print!("{out}");
    std::io::stdout().flush().ok();
}

/// The running service: its pid and memory in kB, found in /proc.
fn service() -> Option<(u32, u64)> {
    let me = std::process::id();
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        if pid == me || std::fs::read_to_string(format!("/proc/{pid}/comm")).map(|c| c.trim() != "herald").unwrap_or(true) {
            continue;
        }
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        let kb = status.lines().find_map(|l| l.strip_prefix("VmRSS:")).and_then(|v| v.split_whitespace().next()?.parse().ok());
        return Some((pid, kb.unwrap_or(0)));
    }
    None
}

fn send_test() -> Result<(), String> {
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let hints: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &("herald", 0u32, "", "A test from herald", "This box closes in six seconds, or click it.", Vec::<&str>::new(), hints, -1i32),
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

pub fn age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86399 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86400),
    }
}

/// At most `max` cells of `s`, never cutting a letter in two.
fn take(s: &str, max: usize) -> String {
    let mut walker = crust::WidthWalker::new();
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let add = walker.push(c);
        if w + add > max {
            break;
        }
        w += add;
        out.push(c);
    }
    out
}
