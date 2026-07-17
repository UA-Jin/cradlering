// Terminal Core module implements safe text behavior.
// 翻译自 packages/terminal-core/src/safe-text.ts

use crate::ansi::strip_ansi;

/**
 * Normalize untrusted text for single-line terminal/log rendering.
 */
pub fn sanitize_terminal_text(input: &str) -> String {
    let normalized = strip_ansi(input)
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace('\t', "\\t");
    let mut sanitized = String::new();
    for ch in normalized.chars() {
        let code = ch as u32;
        #[allow(unused_comparisons)]
        let is_control = (code >= 0x00 && code <= 0x1f) || (code >= 0x7f && code <= 0x9f);
        if !is_control {
            sanitized.push(ch);
        }
    }
    sanitized
}
