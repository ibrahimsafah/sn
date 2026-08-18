//! Fuzzes the record-reference parsers (`table:identifier` argv tokens).
//! These are pure functions over untrusted argv — no network, no filesystem —
//! so anything but a clean `Ok`/`Err` return is a bug.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sn::cli::record_ref::{parse_get_ref, parse_pair, parse_ref};

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = parse_ref(s, "record");
        let _ = parse_get_ref(s);
        // Exercise the two-positional form too: split the input into the
        // (first, second) pair a command would receive.
        match s.split_once('\n') {
            Some((a, b)) => {
                let _ = parse_pair(a, Some(b), "record");
            }
            None => {
                let _ = parse_pair(s, None, "record");
            }
        }
    }
});
