// Terminal Core module implements safe stream writer behavior.
// 翻译自 packages/terminal-core/src/stream-writer.ts

use std::io::Write;

#[derive(Default)]
pub struct SafeStreamWriterOptions {
    pub before_write: Option<Box<dyn Fn() + Send + Sync>>,
    pub on_broken_pipe: Option<Box<dyn Fn(i32, &str) + Send + Sync>>,
}

pub struct SafeStreamWriter {
    closed: bool,
    notified: bool,
    options: SafeStreamWriterOptions,
}

pub trait WriteStreamLike: Write {
    fn fd(&self) -> i32;
}

fn is_broken_pipe_error(code: i32) -> bool {
    code == 32 /* EPIPE */ || code == 5 /* EIO */
}

impl SafeStreamWriter {
    pub fn new(options: SafeStreamWriterOptions) -> Self {
        SafeStreamWriter {
            closed: false,
            notified: false,
            options,
        }
    }

    fn note_broken_pipe(&mut self, err_code: i32, label: &str) {
        if self.notified {
            return;
        }
        self.notified = true;
        if let Some(cb) = &self.options.on_broken_pipe {
            cb(err_code, label);
        }
    }

    fn handle_error(&mut self, err: &std::io::Error, _fd: i32, label: &str) -> bool {
        let raw = err.raw_os_error().unwrap_or(0);
        if is_broken_pipe_error(raw) {
            self.closed = true;
            self.note_broken_pipe(raw, label);
            return false;
        }
        // Re-raise non-pipe errors by panicking — mirrors `throw err` in TS.
        panic!("write failed: {}", err);
    }

    pub fn write_to<W: Write>(&mut self, stream: &mut W, text: &str, label: &str) -> bool {
        if self.closed {
            return false;
        }
        if let Some(before) = &self.options.before_write {
            if let Err(err) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                before()
            })) {
                eprintln!("before_write panicked");
                let _ = err;
                return false;
            }
        }
        match stream.write_all(text.as_bytes()) {
            Ok(()) => !self.closed,
            Err(err) => {
                let raw = err.raw_os_error().unwrap_or(0);
                self.handle_error(&err, raw, label)
            }
        }
    }

    pub fn write_line<W: Write>(&mut self, stream: &mut W, text: &str, label: &str) -> bool {
        let mut with_nl = String::with_capacity(text.len() + 1);
        with_nl.push_str(text);
        with_nl.push('\n');
        self.write_to(stream, &with_nl, label)
    }

    pub fn reset(&mut self) {
        self.closed = false;
        self.notified = false;
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

/// Create a stream writer that stops writing after EPIPE/EIO.
pub fn create_safe_stream_writer(options: SafeStreamWriterOptions) -> SafeStreamWriter {
    SafeStreamWriter::new(options)
}
