//! ~/.heraldrc: `key = value` lines, `#` for a comment. Every key is
//! optional; the defaults are the look herald took over from dunst.

pub struct Config {
    pub font: String,
    pub font_bold: String,
    pub font_size: f32,
    /// Box width in pixels.
    pub width: u16,
    /// Gap from the screen's right edge and from its top.
    pub x: u16,
    pub y: u16,
    /// Space between two boxes.
    pub gap: u16,
    pub padding: u16,
    pub frame: u16,
    /// (frame and text colour, background) for low, normal, critical.
    pub low: (u32, u32),
    pub normal: (u32, u32),
    pub critical: (u32, u32),
    /// Seconds on screen; 0 means until clicked.
    pub timeouts: [u64; 3],
    /// Titles that go to the history but never on screen.
    pub skip: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            font: "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf".into(),
            font_bold: "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf".into(),
            font_size: 15.0,
            width: 360,
            x: 0,
            y: 40,
            gap: 4,
            padding: 6,
            frame: 2,
            low: (0x3B7C87, 0x191311),
            normal: (0x5B8234, 0x191311),
            critical: (0xB7472A, 0x191311),
            timeouts: [4, 6, 8],
            skip: Vec::new(),
        }
    }
}

impl Config {
    pub fn load() -> Config {
        let home = std::env::var("HOME").unwrap_or_default();
        let text = std::fs::read_to_string(format!("{home}/.heraldrc")).unwrap_or_default();
        Config::parse(&text)
    }

    pub fn parse(text: &str) -> Config {
        let mut c = Config::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim().trim_matches('"'));
            let num = |d: u16| v.parse().unwrap_or(d);
            let col = |d: u32| u32::from_str_radix(v.trim_start_matches('#'), 16).unwrap_or(d);
            match k {
                "font" => c.font = v.into(),
                "font_bold" => c.font_bold = v.into(),
                "font_size" => c.font_size = v.parse().unwrap_or(c.font_size),
                "width" => c.width = num(c.width),
                "x" => c.x = num(c.x),
                "y" => c.y = num(c.y),
                "gap" => c.gap = num(c.gap),
                "padding" => c.padding = num(c.padding),
                "frame" => c.frame = num(c.frame),
                "low_fg" => c.low.0 = col(c.low.0),
                "low_bg" => c.low.1 = col(c.low.1),
                "normal_fg" => c.normal.0 = col(c.normal.0),
                "normal_bg" => c.normal.1 = col(c.normal.1),
                "critical_fg" => c.critical.0 = col(c.critical.0),
                "critical_bg" => c.critical.1 = col(c.critical.1),
                "low_timeout" => c.timeouts[0] = v.parse().unwrap_or(c.timeouts[0]),
                "normal_timeout" => c.timeouts[1] = v.parse().unwrap_or(c.timeouts[1]),
                "critical_timeout" => c.timeouts[2] = v.parse().unwrap_or(c.timeouts[2]),
                "skip" => c.skip.push(v.to_string()),
                _ => {}
            }
        }
        c
    }

    pub fn colors(&self, urgency: u8) -> (u32, u32) {
        match urgency {
            0 => self.low,
            2 => self.critical,
            _ => self.normal,
        }
    }

    /// Seconds a box of this urgency stays, or None to stay until clicked.
    pub fn timeout(&self, urgency: u8) -> Option<u64> {
        let s = self.timeouts[urgency.min(2) as usize];
        (s > 0).then_some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_read_and_defaults_stay() {
        let c = Config::parse("# c\nwidth = 400\nnormal_fg = #112233\nskip = Claude Code\ncritical_timeout = 0\n");
        assert_eq!(c.width, 400);
        assert_eq!(c.normal.0, 0x112233);
        assert_eq!(c.normal.1, 0x191311);
        assert_eq!(c.skip, ["Claude Code"]);
        assert_eq!(c.timeout(2), None);
        assert_eq!(c.timeout(1), Some(6));
    }
}
