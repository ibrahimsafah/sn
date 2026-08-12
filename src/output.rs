//! Two ways out of this process, and the difference is not cosmetic.
//!
//! **Batch** — one finished payload whose end is already known (a single record,
//! an `--array` buffer). Nothing is waiting on a partial result, so it goes
//! through [`write_value`], which coalesces the write into 64 KiB blocks.
//!
//! **Stream** — records produced over time by a paginator or a websocket, with a
//! reader on the far end of a pipe. Those go through [`write_jsonl_line`], one
//! call per record, which flushes after **every** record: a consumer cannot
//! distinguish a line sitting in our buffer from a process that has hung, so
//! buffering a stream turns `sn watch | jq` into something that looks frozen.
//! There is deliberately no whole-iterator JSONL helper — one existed, nothing
//! ever called it, and its tests read like coverage of the streaming path while
//! pinning a function that was dead in the binary.
//!
//! The split lives in the functions rather than in a comment at the call sites,
//! because the call sites are where it was previously getting lost.

use crate::error::{Error, Result};
use is_terminal::IsTerminal;
use serde_json::Value;
use std::io::{self, BufWriter, ErrorKind, Write};

/// stdout is a `LineWriter`: it syscalls on every `\n`, and compact JSON has no
/// newline at all, so its ~1 KiB buffer is what bounds a batch write. A 10 MB
/// `--all --array` result left in roughly ten thousand writes.
const BATCH_CAPACITY: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Pretty-printed when stdout is a TTY, compact when piped.
    Auto,
    /// Always pretty.
    Pretty,
    /// Always compact (single-line).
    Compact,
}

impl Format {
    pub fn resolve(self) -> ResolvedFormat {
        match self {
            Format::Pretty => ResolvedFormat::Pretty,
            Format::Compact => ResolvedFormat::Compact,
            Format::Auto => {
                if io::stdout().is_terminal() {
                    ResolvedFormat::Pretty
                } else {
                    ResolvedFormat::Compact
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedFormat {
    Pretty,
    Compact,
}

/// Emit a single JSON value to stdout, trailing newline.
pub fn emit_value<W: Write>(mut w: W, value: &Value, fmt: ResolvedFormat) -> io::Result<()> {
    match fmt {
        ResolvedFormat::Pretty => serde_json::to_writer_pretty(&mut w, value)?,
        ResolvedFormat::Compact => serde_json::to_writer(&mut w, value)?,
    }
    w.write_all(b"\n")
}

/// Emit an error to stderr as the documented JSON envelope.
pub fn emit_error<W: Write>(mut w: W, err: &Error) -> io::Result<()> {
    serde_json::to_writer(&mut w, &err.to_stderr_json())?;
    w.write_all(b"\n")
}

/// Map an I/O error from writing to stdout into the right `Error` variant.
/// `BrokenPipe` becomes `Error::BrokenPipe` (silent exit 0); everything else
/// is a transport failure (exit 3), not a usage error.
pub fn map_stdout_err(e: io::Error) -> Error {
    if e.kind() == ErrorKind::BrokenPipe {
        Error::BrokenPipe
    } else {
        Error::Transport(format!("stdout: {e}"))
    }
}

/// Run `f` against a batched writer and flush it.
///
/// The flush is explicit and its error is propagated because `BufWriter`'s
/// `Drop` throws a failing flush away: a closed pipe or a full disk would
/// truncate the payload and the command would still exit 0.
fn write_batched<W: Write>(
    inner: W,
    f: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<()> {
    let mut w = BufWriter::with_capacity(BATCH_CAPACITY, inner);
    f(&mut w).map_err(map_stdout_err)?;
    w.flush().map_err(map_stdout_err)
}

/// Write one finished value to stdout — the batch path, buffered. Broken pipes
/// stay clean (`Error::BrokenPipe`, exit 0). Never reach for this from a loop
/// over live records; that is what [`write_jsonl_line`] is for.
pub fn write_value(value: &Value, fmt: ResolvedFormat) -> Result<()> {
    write_batched(io::stdout().lock(), |w| emit_value(w, value, fmt))
}

/// Write a single JSON record + newline to a writer and flush it — the stream
/// path, one record at a time. Broken pipes become `Error::BrokenPipe`; other
/// I/O becomes `Error::Transport`. The flush is part of the contract, not an
/// artifact of stdout being line-buffered: callers pass their own writer.
pub fn write_jsonl_line<W: Write>(mut w: W, v: &Value) -> Result<()> {
    serde_json::to_writer(&mut w, v).map_err(|e| {
        if e.is_io() {
            Error::BrokenPipe
        } else {
            Error::Transport(format!("serialize: {e}"))
        }
    })?;
    w.write_all(b"\n").map_err(map_stdout_err)?;
    w.flush().map_err(map_stdout_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Stands in for a pipe: `visible` is what a reader on the far end could
    /// have read, so bytes only move there on flush. `writes` counts syscalls.
    #[derive(Default)]
    struct Pipe {
        pending: Vec<u8>,
        visible: Vec<u8>,
        writes: usize,
        flush_err: Option<ErrorKind>,
    }

    struct SpyWriter(Rc<RefCell<Pipe>>);

    impl Write for SpyWriter {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            let mut p = self.0.borrow_mut();
            p.writes += 1;
            p.pending.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            let mut p = self.0.borrow_mut();
            if let Some(kind) = p.flush_err {
                return Err(io::Error::new(kind, "spy flush failure"));
            }
            let pending = std::mem::take(&mut p.pending);
            p.visible.extend_from_slice(&pending);
            Ok(())
        }
    }

    fn visible(pipe: &Rc<RefCell<Pipe>>) -> String {
        String::from_utf8(pipe.borrow().visible.clone()).unwrap()
    }

    #[test]
    fn compact_emits_single_line() {
        let mut buf = Vec::new();
        emit_value(&mut buf, &json!({"a": 1}), ResolvedFormat::Compact).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "{\"a\":1}\n");
    }

    #[test]
    fn pretty_emits_indented() {
        let mut buf = Vec::new();
        emit_value(&mut buf, &json!({"a": 1}), ResolvedFormat::Pretty).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("  \"a\": 1"));
    }

    #[test]
    fn jsonl_one_record_per_line() {
        let mut buf = Vec::new();
        for v in [json!({"a": 1}), json!({"a": 2})] {
            write_jsonl_line(&mut buf, &v).unwrap();
        }
        assert_eq!(String::from_utf8(buf).unwrap(), "{\"a\":1}\n{\"a\":2}\n");
    }

    /// The stream contract: a record has reached the reader before the iterator
    /// is asked for the next one. If JSONL were ever wrapped in a `BufWriter`,
    /// nothing would be visible until 64 KiB accumulated and this fails.
    ///
    /// The loop is the shape of the real streaming call sites — `cli::table`'s
    /// `--all` and `cli::watch`'s event loop both call [`write_jsonl_line`]
    /// once per record off a live producer — so this exercises the code the
    /// binary actually runs.
    #[test]
    fn jsonl_records_are_visible_before_the_next_is_produced() {
        let pipe = Rc::new(RefCell::new(Pipe::default()));
        let seen = Rc::clone(&pipe);
        let mut produced = 0usize;
        let it = std::iter::from_fn(move || {
            assert_eq!(
                visible(&seen).lines().count(),
                produced,
                "record {produced} was still buffered when the next was requested"
            );
            if produced == 3 {
                return None;
            }
            produced += 1;
            Some(json!({ "i": produced }))
        });
        let mut w = SpyWriter(Rc::clone(&pipe));
        for v in it {
            write_jsonl_line(&mut w, &v).unwrap();
        }
        assert_eq!(visible(&pipe).lines().count(), 3);
    }

    #[test]
    fn a_streamed_line_is_flushed_on_its_own() {
        let pipe = Rc::new(RefCell::new(Pipe::default()));
        write_jsonl_line(SpyWriter(Rc::clone(&pipe)), &json!({"a": 1})).unwrap();
        assert_eq!(visible(&pipe), "{\"a\":1}\n");
    }

    /// The batch contract: serde's pretty printer emits a write per token, and
    /// stdout would syscall on each of its newlines. One coalesced write out.
    #[test]
    fn a_batched_value_leaves_in_a_single_write() {
        let pipe = Rc::new(RefCell::new(Pipe::default()));
        let value = json!({"rows": (0..500).map(|i| json!({"n": i})).collect::<Vec<_>>()});
        write_batched(SpyWriter(Rc::clone(&pipe)), |w| {
            emit_value(w, &value, ResolvedFormat::Pretty)
        })
        .unwrap();
        let p = pipe.borrow();
        assert_eq!(
            p.writes, 1,
            "batched output fragmented into {} writes",
            p.writes
        );
        assert!(p.visible.len() > BATCH_CAPACITY / 8);
    }

    /// `BufWriter::drop` discards a failing flush. If the batch path ever
    /// relied on that drop, a full disk would truncate stdout and exit 0.
    #[test]
    fn a_deferred_flush_failure_is_not_swallowed() {
        let pipe = Rc::new(RefCell::new(Pipe {
            flush_err: Some(ErrorKind::Other),
            ..Pipe::default()
        }));
        let err = write_batched(SpyWriter(Rc::clone(&pipe)), |w| {
            emit_value(w, &json!({"a": 1}), ResolvedFormat::Compact)
        })
        .unwrap_err();
        assert!(
            matches!(err, Error::Transport(_)),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn a_closed_pipe_on_flush_stays_a_broken_pipe() {
        let pipe = Rc::new(RefCell::new(Pipe {
            flush_err: Some(ErrorKind::BrokenPipe),
            ..Pipe::default()
        }));
        let err = write_batched(SpyWriter(Rc::clone(&pipe)), |w| {
            emit_value(w, &json!({"a": 1}), ResolvedFormat::Compact)
        })
        .unwrap_err();
        assert!(matches!(err, Error::BrokenPipe), "unexpected error: {err}");
    }

    #[test]
    fn error_envelope_goes_to_writer() {
        let e = Error::Usage("bad".into());
        let mut buf = Vec::new();
        emit_error(&mut buf, &e).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"message\":\"bad\""));
    }
}
