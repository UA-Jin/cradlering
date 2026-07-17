// Terminal Core module implements theme behavior.
// 翻译自 packages/terminal-core/src/theme.ts

use std::sync::atomic::{AtomicI32, Ordering};

use crate::palette::LOBSTER_PALETTE;

/// Wrapper that remembers whether rich-styling (chalk-style hex color) is enabled.
/// In CradleRing we expose the same shape as the TS module: a Colorizer trait
/// for `hex` color functions plus the global theme record.
pub trait Colorizer: Send + Sync {
    fn hex(&self, value: &str) -> String;
    fn bold(&self) -> Box<dyn Colorizer>;
}

pub struct HexColorizer;

impl Colorizer for HexColorizer {
    fn hex(&self, value: &str) -> String {
        value.to_string()
    }
    fn bold(&self) -> Box<dyn Colorizer> {
        Box::new(HexColorizer)
    }
}

pub struct NoColorColorizer;

impl Colorizer for NoColorColorizer {
    fn hex(&self, value: &str) -> String {
        value.to_string()
    }
    fn bold(&self) -> Box<dyn Colorizer> {
        Box::new(NoColorColorizer)
    }
}

pub struct HexColorizerBold {
    hex_prefix: String,
}

impl Colorizer for HexColorizerBold {
    fn hex(&self, value: &str) -> String {
        format!("{}{}\u{001B}[0m", self.hex_prefix, value)
    }
    fn bold(&self) -> Box<dyn Colorizer> {
        Box::new(HexColorizerBold {
            hex_prefix: self.hex_prefix.clone(),
        })
    }
}

fn hex_color(hex: &str, bold: bool) -> Box<dyn Colorizer> {
    let prefix = format!("\x1b[{};2;{}m", if bold { 1 } else { 0 }, rgb_from_hex(hex));
    Box::new(HexColorizerBold { hex_prefix: prefix })
}

fn rgb_from_hex(hex: &str) -> String {
    let s = hex.trim_start_matches('#');
    if s.len() != 6 {
        return "0;0;0".to_string();
    }
    let r = u32::from_str_radix(&s[0..2], 16).unwrap_or(0);
    let g = u32::from_str_radix(&s[2..4], 16).unwrap_or(0);
    let b = u32::from_str_radix(&s[4..6], 16).unwrap_or(0);
    format!("{};{};{}", r, g, b)
}

static RICH_LEVEL: AtomicI32 = AtomicI32::new(-1);

#[allow(dead_code)]
fn base_colorizer() -> Box<dyn Colorizer> {
    if RICH_LEVEL.load(Ordering::SeqCst) > 0 {
        Box::new(HexColorizer)
    } else {
        Box::new(NoColorColorizer)
    }
}

/// Determine if color styling is active. Reflects the OPENCLAW_NO_COLOR /
/// FORCE_COLOR environment conventions. Callers can override the level with
/// [`set_rich_level`].
pub fn is_rich() -> bool {
    let v = RICH_LEVEL.load(Ordering::SeqCst);
    if v >= 0 {
        return v > 0;
    }
    let force = std::env::var("FORCE_COLOR")
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let force_active = !force.is_empty() && force != "0";
    let has_force_color = force_active;
    let no_color = std::env::var("NO_COLOR").ok().filter(|v| !v.is_empty()).is_some();
    let rich = (has_force_color || !no_color) && atty_stdout_is_tty();
    rich
}

fn atty_stdout_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

pub fn set_rich_level(level: i32) {
    RICH_LEVEL.store(level, Ordering::SeqCst);
}

pub struct Theme {
    pub accent: Box<dyn Colorizer>,
    pub accent_bright: Box<dyn Colorizer>,
    pub accent_dim: Box<dyn Colorizer>,
    pub info: Box<dyn Colorizer>,
    pub success: Box<dyn Colorizer>,
    pub warn: Box<dyn Colorizer>,
    pub error: Box<dyn Colorizer>,
    pub muted: Box<dyn Colorizer>,
    pub heading: Box<dyn Colorizer>,
    pub command: Box<dyn Colorizer>,
    pub option: Box<dyn Colorizer>,
}

pub fn theme() -> Theme {
    if is_rich() {
        Theme {
            accent: hex_color(LOBSTER_PALETTE.accent, false),
            accent_bright: hex_color(LOBSTER_PALETTE.accent_bright, false),
            accent_dim: hex_color(LOBSTER_PALETTE.accent_dim, false),
            info: hex_color(LOBSTER_PALETTE.info, false),
            success: hex_color(LOBSTER_PALETTE.success, false),
            warn: hex_color(LOBSTER_PALETTE.warn, false),
            error: hex_color(LOBSTER_PALETTE.error, false),
            muted: hex_color(LOBSTER_PALETTE.muted, false),
            heading: hex_color(LOBSTER_PALETTE.accent, true),
            command: hex_color(LOBSTER_PALETTE.accent_bright, false),
            option: hex_color(LOBSTER_PALETTE.warn, false),
        }
    } else {
        Theme {
            accent: Box::new(NoColorColorizer),
            accent_bright: Box::new(NoColorColorizer),
            accent_dim: Box::new(NoColorColorizer),
            info: Box::new(NoColorColorizer),
            success: Box::new(NoColorColorizer),
            warn: Box::new(NoColorColorizer),
            error: Box::new(NoColorColorizer),
            muted: Box::new(NoColorColorizer),
            heading: Box::new(NoColorColorizer),
            command: Box::new(NoColorColorizer),
            option: Box::new(NoColorColorizer),
        }
    }
}

/// Conditionally apply a color function based on caller rich-output state.
pub fn colorize(rich: bool, color: &dyn Colorizer, value: &str) -> String {
    if rich {
        color.hex(value)
    } else {
        value.to_string()
    }
}
