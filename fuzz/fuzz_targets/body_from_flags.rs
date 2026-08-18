//! Fuzzes `--data`/`--field` body construction — the JSON and `key=value`
//! parsing every write command funnels untrusted argv through.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sn::body::{from_flags, EmptyBody};

fuzz_target!(|input: (Option<String>, Vec<String>)| {
    let (data, fields) = input;
    // A leading `@` makes --data read a file (or stdin). The parser's job
    // here is syntax, not the filesystem, so those inputs are skipped.
    if data.as_deref().is_some_and(|d| d.starts_with('@')) {
        return;
    }
    let _ = from_flags(data, fields, EmptyBody::Object);
});
