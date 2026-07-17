// Terminal Core module implements terminal link formatting.
// 翻译自 packages/terminal-core/src/terminal-link.ts

fn strip_terminal_link_controls(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        let code = ch as u32;
        #[allow(unused_comparisons)]
        let is_control = (code >= 0x00 && code <= 0x1f) || (code >= 0x7f && code <= 0x9f);
        if !is_control {
            out.push(ch);
        }
    }
    out
}

#[derive(Default, Clone, Debug)]
pub struct FormatTerminalLinkOptions {
    pub fallback: Option<String>,
    pub force: Option<bool>,
}

/// Format a clickable terminal link when supported, otherwise return a readable fallback.
pub fn format_terminal_link(label: &str, url: &str, opts: Option<FormatTerminalLinkOptions>) -> String {
    let opts = opts.unwrap_or_default();
    let safe_label = strip_terminal_link_controls(label);
    let safe_url = strip_terminal_link_controls(url);
    let allow = match opts.force {
        Some(true) => true,
        Some(false) => false,
        None => stdout_is_tty(),
    };
    if !allow {
        return match opts.fallback {
            Some(fallback) => strip_terminal_link_controls(&fallback),
            None => format!("{} ({})", safe_label, safe_url),
        };
    }
    format!("\u{001B}]8;;{}\u{0007}{}\u{001B}]8;;\u{0007}", safe_url, safe_label)
}

fn stdout_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}
