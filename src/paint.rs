//! A notification drawn into pixels: a frame, the title in bold, the
//! text under it, wrapped to the box. frame's X server has no fonts of
//! its own, so herald draws the letters itself with ab_glyph, which
//! reads a letter from the font file only when it is first drawn.

use crate::config::Config;
use crate::Note;
use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use std::cell::RefCell;
use std::collections::HashMap;

/// At most this many lines of text in one box.
const MAX_LINES: usize = 12;

/// A drawn letter: where its box starts from the pen and the baseline,
/// its size, and how much of each pixel it covers (0..255).
struct Glyph {
    dx: i32,
    dy: i32,
    w: usize,
    h: usize,
    cover: Vec<u8>,
}

pub struct Painter {
    regular: FontVec,
    bold: Option<FontVec>,
    scale: PxScale,
    /// Advance of one letter; the font is monospaced.
    adv: usize,
    ascent: usize,
    line_h: usize,
    width: u16,
    padding: usize,
    frame: usize,
    colors: [(u32, u32); 3],
    cache: RefCell<HashMap<(char, bool), Glyph>>,
}

impl Painter {
    pub fn new(cfg: &Config) -> Result<Painter, String> {
        let load = |p: &str| -> Result<FontVec, String> {
            let bytes = std::fs::read(p).map_err(|e| format!("cannot read font {p}: {e}"))?;
            FontVec::try_from_vec(bytes).map_err(|e| format!("bad font {p}: {e}"))
        };
        let regular = load(&cfg.font)?;
        let bold = load(&cfg.font_bold).ok();
        let scale = PxScale::from(cfg.font_size);
        let sf = regular.as_scaled(scale);
        let adv = sf.h_advance(regular.glyph_id('M')).ceil() as usize;
        Ok(Painter {
            scale,
            adv: adv.max(1),
            ascent: sf.ascent().ceil() as usize,
            line_h: (sf.ascent() - sf.descent() + sf.line_gap()).ceil() as usize,
            width: cfg.width,
            padding: cfg.padding as usize,
            frame: cfg.frame as usize,
            colors: [cfg.low, cfg.normal, cfg.critical],
            regular,
            bold,
            cache: RefCell::new(HashMap::new()),
        })
    }

    /// The box as (width, height, pixels in the X server's BGRX order).
    pub fn paint(&self, n: &Note) -> (u16, u16, Vec<u8>) {
        let (fg, bg) = self.colors[n.urgency.min(2) as usize];
        let inner = self.width as usize - 2 * (self.frame + self.padding);
        let cols = (inner / self.adv).max(1);
        let mut lines: Vec<(String, bool)> = wrap(&plain(&n.summary), cols).into_iter().map(|l| (l, true)).collect();
        lines.extend(wrap(&plain(&n.body), cols).into_iter().map(|l| (l, false)));
        lines.truncate(MAX_LINES);
        if lines.is_empty() {
            lines.push((plain(&n.app), true));
        }
        let w = self.width as usize;
        let h = 2 * (self.frame + self.padding) + lines.len() * self.line_h;
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let edge = x < self.frame || y < self.frame || x >= w - self.frame || y >= h - self.frame;
                set(&mut px, w, x, y, if edge { fg } else { bg });
            }
        }
        for (i, (text, bold)) in lines.iter().enumerate() {
            let base = self.frame + self.padding + i * self.line_h + self.ascent;
            let mut pen = self.frame + self.padding;
            for c in text.chars() {
                self.glyph(&mut px, w, h, c, *bold, pen, base, fg, bg);
                pen += self.adv;
            }
        }
        (w as u16, h as u16, px)
    }

    #[allow(clippy::too_many_arguments)]
    fn glyph(&self, px: &mut [u8], w: usize, h: usize, c: char, bold: bool, pen: usize, base: usize, fg: u32, bg: u32) {
        let mut cache = self.cache.borrow_mut();
        let g = cache.entry((c, bold)).or_insert_with(|| {
            let font = if bold { self.bold.as_ref().unwrap_or(&self.regular) } else { &self.regular };
            let glyph = font.glyph_id(c).with_scale(self.scale);
            let Some(outline) = font.outline_glyph(glyph) else {
                return Glyph { dx: 0, dy: 0, w: 0, h: 0, cover: Vec::new() };
            };
            let b = outline.px_bounds();
            let (gw, gh) = (b.width() as usize, b.height() as usize);
            let mut cover = vec![0u8; gw * gh];
            outline.draw(|x, y, v| {
                let i = y as usize * gw + x as usize;
                if i < cover.len() {
                    cover[i] = (v.clamp(0.0, 1.0) * 255.0) as u8;
                }
            });
            Glyph { dx: b.min.x as i32, dy: b.min.y as i32, w: gw, h: gh, cover }
        });
        for gy in 0..g.h {
            for gx in 0..g.w {
                let a = g.cover[gy * g.w + gx] as u32;
                if a == 0 {
                    continue;
                }
                let x = pen as i32 + g.dx + gx as i32;
                let y = base as i32 + g.dy + gy as i32;
                if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                    continue;
                }
                set(px, w, x as usize, y as usize, blend(fg, bg, a));
            }
        }
    }
}

fn set(px: &mut [u8], w: usize, x: usize, y: usize, rgb: u32) {
    let i = (y * w + x) * 4;
    px[i] = rgb as u8;
    px[i + 1] = (rgb >> 8) as u8;
    px[i + 2] = (rgb >> 16) as u8;
}

/// `fg` laid over `bg` at coverage `a` (0..255).
fn blend(fg: u32, bg: u32, a: u32) -> u32 {
    let ch = |s: u32| {
        let f = (fg >> s) & 0xff;
        let b = (bg >> s) & 0xff;
        ((f * a + b * (255 - a)) / 255) << s
    };
    ch(0) | ch(8) | ch(16)
}

/// Text without the markup some senders use (<b>, <i>, <a href=…>),
/// with the five named characters turned back into themselves.
pub fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Lines of at most `cols` letters, broken between words where it can.
/// A newline in the text starts a new line.
pub fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let mut word = word.to_string();
            while word.chars().count() > cols {
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                let head: String = word.chars().take(cols).collect();
                word = word.chars().skip(cols).collect();
                out.push(head);
            }
            let len = line.chars().count();
            if len > 0 && len + 1 + word.chars().count() > cols {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&word);
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_goes_and_named_characters_come_back() {
        assert_eq!(plain("<b>Mail</b> from A &amp; B &lt;x&gt;"), "Mail from A & B <x>");
    }

    #[test]
    fn lines_break_between_words_and_split_long_ones() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a\nb", 10), ["a", "b"]);
    }

    #[test]
    fn half_cover_lands_half_way() {
        assert_eq!(blend(0xff0000, 0x000000, 255), 0xff0000);
        assert_eq!(blend(0xff0000, 0x000000, 0), 0);
        assert_eq!(blend(0xc8c8c8, 0x000000, 128) & 0xff, 100);
    }
}
