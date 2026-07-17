// Terminal Core module implements restore behavior.
// 翻译自 packages/terminal-core/src/restore.ts

use crate::progress_line::clear_active_progress_line;

const RESET_SEQUENCE: &str = "\u{001B}[0m\u{001B}[?25h\u{001B}[?1000l\u{001B}[?1002l\u{001B}[?1003l\u{001B}[?1006l\u{001B}[?2004l\u{001B}[<u\u{001B}[>4;0m";

#[derive(Default, Clone, Copy, Debug)]
pub struct RestoreTerminalStateOptions {
    /// Resumes paused stdin after restoring terminal mode.
    pub resume_stdin: Option<bool>,
    /// Alias for resume_stdin. Prefer this name to make the behavior explicit.
    pub resume_stdin_if_paused: Option<bool>,
}

fn report_restore_failure(scope: &str, err: &dyn std::error::Error, reason: Option<&str>) {
    let suffix = reason.map(|r| format!(" ({})", r)).unwrap_or_default();
    let message = format!("[terminal] restore {} failed{}: {}", scope, suffix, err);
    let stderr = std::io::stderr();
    use std::io::Write;
    let _ = writeln!(stderr.lock(), "{}", message);
}

pub fn restore_terminal_state(reason: Option<&str>, options: Option<RestoreTerminalStateOptions>) {
    let options = options.unwrap_or_default();
    // Docker TTY note: resuming stdin can keep a container process alive even
    // after the wizard is "done" (stdin_open: true), making installers appear hung.
    let resume_stdin = options
        .resume_stdin_if_paused
        .or(options.resume_stdin)
        .unwrap_or(false);

    if let Err(err) = std::panic::catch_unwind(|| {
        clear_active_progress_line();
    }) {
        if let Some(s) = err.downcast_ref::<&str>() {
            report_restore_failure("progress line", &std::io::Error::new(std::io::ErrorKind::Other, *s), reason);
        }
    }

    // We do not perform raw-mode toggles here: this is intentionally a
    // best-effort terminal state restoration in a sync handler context. A real
    // host provides its own stdin/raw-mode policy.
    let _ = resume_stdin;

    // Reset visible terminal state via stdout when it is a TTY.
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        if let Err(err) = stdout.write_all(RESET_SEQUENCE.as_bytes()) {
            report_restore_failure("stdout reset", &err, reason);
        }
    }
}
