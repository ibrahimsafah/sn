use crate::cli::kernel::{confirm_delete, connect, emit, write_response};
use crate::cli::{GlobalFlags, Paging};
use crate::client::{Download, DownloadError};
use crate::error::{Error, Result};
use clap::Subcommand;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Subcommand, Debug)]
pub enum AttachmentSub {
    /// List attachments with optional query filter.
    List(AttachmentListArgs),
    /// Get attachment metadata by sys_id.
    Get(AttachmentGetArgs),
    /// Upload a file as an attachment.
    Upload(AttachmentUploadArgs),
    /// Download attachment content to a file or stdout.
    Download(AttachmentDownloadArgs),
    /// Delete an attachment.
    Delete(AttachmentDeleteArgs),
}

#[derive(clap::Args, Debug)]
pub struct AttachmentListArgs {
    /// Encoded query, e.g. `active=true^priority=1`.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    pub query: Option<String>,
    #[command(flatten)]
    pub paging: Paging<100>,
}

#[derive(clap::Args, Debug)]
pub struct AttachmentGetArgs {
    /// sys_id of the attachment.
    pub sys_id: String,
}

#[derive(clap::Args, Debug)]
pub struct AttachmentUploadArgs {
    /// Table to attach to (e.g. `incident`).
    #[arg(long, required = true)]
    pub table: String,
    /// sys_id of the record to attach to.
    #[arg(long, required = true)]
    pub record: String,
    /// Path to the file to upload.
    #[arg(long, required = true)]
    pub file: String,
    /// File name override (defaults to the file's basename).
    #[arg(long)]
    pub file_name: Option<String>,
    /// Content type (auto-detected if not specified).
    #[arg(long)]
    pub content_type: Option<String>,
    /// Encryption context sys_id.
    #[arg(long)]
    pub encryption_context: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct AttachmentDownloadArgs {
    /// sys_id of the attachment.
    pub sys_id: String,
    /// Write the file here. Defaults to stdout.
    ///
    /// NOT `--output`: that name belongs to the global `--output default|raw|table`.
    /// clap merges args by id, so a second `output` here shadowed the global one's
    /// type and made `GlobalFlags` downcast an `OutputMode` out of a `String` —
    /// panicking on *every* `attachment download`, with or without the flag. Keep
    /// this id distinct from any global.
    #[arg(long = "out", short = 'o', value_name = "PATH")]
    pub out: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct AttachmentDeleteArgs {
    /// sys_id of the attachment.
    pub sys_id: String,
    /// Skip confirmation prompt (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
}

pub fn list(global: &GlobalFlags, args: AttachmentListArgs) -> Result<()> {
    let client = connect(global)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.query {
        query.push(("sysparm_query".into(), v));
    }
    query.push(("sysparm_limit".into(), args.paging.setlimit().to_string()));
    if let Some(v) = args.paging.offset {
        query.push(("sysparm_offset".into(), v.to_string()));
    }
    let resp = client.get("/api/now/attachment", &query)?;
    emit(global, resp)
}

pub fn get(global: &GlobalFlags, args: AttachmentGetArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/now/attachment/{}", args.sys_id);
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}

pub fn upload(global: &GlobalFlags, args: AttachmentUploadArgs) -> Result<()> {
    let client = connect(global)?;
    let file_path = Path::new(&args.file);
    let file_name = args.file_name.unwrap_or_else(|| {
        file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("upload")
            .to_string()
    });
    let content_type = args
        .content_type
        .unwrap_or_else(|| mime_from_extension(file_path).to_string());
    let body =
        std::fs::read(file_path).map_err(|e| Error::Usage(format!("read {}: {e}", args.file)))?;
    let mut query: Vec<(String, String)> = vec![
        ("table_name".into(), args.table),
        ("table_sys_id".into(), args.record),
        ("file_name".into(), file_name),
    ];
    if let Some(v) = args.encryption_context {
        query.push(("encryption_context".into(), v));
    }
    let resp = client.upload_file("/api/now/attachment/file", &query, body, &content_type)?;
    emit(global, resp)
}

pub fn download(global: &GlobalFlags, args: AttachmentDownloadArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/now/attachment/{}/file", args.sys_id);
    let mut download = client.download_file(&path)?;
    match args.out {
        Some(out_path) => stream_to_file(global, &mut download, &out_path),
        None => stream_to_stdout(&mut download),
    }
}

/// Stream the body into a sibling temp file and rename it onto `out_path` only
/// once the transfer completed.
///
/// The destination path must never hold a truncated file. A download that died
/// at 90% and left its bytes under the name the caller asked for is
/// indistinguishable from a good one — nothing downstream can tell, and the
/// corruption surfaces much later somewhere confusing. Writing to a temp file
/// makes the failure loud (nothing at the destination) instead of silent.
///
/// The temp file lives in the *destination's own directory* so the final rename
/// is same-filesystem and therefore atomic; staging in the system temp dir would
/// degrade the rename into a cross-device copy that can itself half-fail,
/// reintroducing exactly the truncation this avoids.
fn stream_to_file(global: &GlobalFlags, download: &mut Download, out_path: &str) -> Result<()> {
    let dest = Path::new(out_path);
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temp_path = dir.join(temp_file_name(dest));

    let mut file = fs::File::create(&temp_path)
        .map_err(|e| Error::Usage(format!("create {}: {e}", temp_path.display())))?;
    reap_staging_on_sigint(&temp_path);
    let written = match download.copy_to(&mut file) {
        Ok(n) => n,
        Err(e) => {
            drop(file);
            let _ = fs::remove_file(&temp_path);
            return Err(discarded(e, out_path));
        }
    };
    if let Err(e) = file.flush() {
        drop(file);
        let _ = fs::remove_file(&temp_path);
        return Err(discarded(DownloadError::Sink(e), out_path));
    }
    // Close before renaming: Windows refuses to move a file that is still open.
    drop(file);
    fs::rename(&temp_path, dest).map_err(|e| {
        let _ = fs::remove_file(&temp_path);
        Error::Usage(format!("write {out_path}: {e}"))
    })?;

    let meta = serde_json::json!({
        "path": out_path,
        "size": written,
    });
    write_response(global, &meta)
}

/// Stream the body to stdout.
///
/// stdout has no staging area: bytes handed to a pipe cannot be recalled, so a
/// mid-stream failure leaves truncated output in the consumer's hands and no
/// amount of cleanup here can undo it. The only honest signal left is refusing
/// to exit 0 — the error envelope on stderr (naming how many bytes were already
/// emitted) plus a nonzero exit. `--out` is the safe form for anything large.
fn stream_to_stdout(download: &mut Download) -> Result<()> {
    let stdout = io::stdout();
    let mut sink = stdout.lock();
    match download.copy_to(&mut sink) {
        Ok(_) => sink.flush().map_err(crate::output::map_stdout_err),
        Err(DownloadError::Sink(e)) => Err(crate::output::map_stdout_err(e)),
        Err(DownloadError::Source(e)) => {
            let written = download.bytes_written();
            Err(Error::Transport(format!(
                "{}; {written} bytes of a truncated download were already written to stdout",
                transport_detail(e)
            )))
        }
    }
}

/// Turn a failed file download into an error that says the destination was left
/// untouched — the caller must not have to guess whether a retry is safe.
fn discarded(e: DownloadError, out_path: &str) -> Error {
    match e {
        DownloadError::Source(err) => Error::Transport(format!(
            "{}; partial download discarded, {out_path} not written",
            transport_detail(err)
        )),
        DownloadError::Sink(err) => Error::Usage(format!(
            "write {out_path}: {err}; partial download discarded, {out_path} not written"
        )),
    }
}

/// The bare message inside a transport error.
///
/// `Error::Transport`'s `Display` is `"transport error: {0}"`, but the stderr
/// envelope emits the *inner* string — so interpolating the error value itself
/// into a new `Error::Transport` prints the variant name inside the message
/// (`"transport error: read body: …"`), a prefix no other sn command emits.
/// Destructure instead; a non-transport variant (which `stream_body` never
/// produces) falls back to `Display` rather than being silently dropped.
fn transport_detail(err: Error) -> String {
    match err {
        Error::Transport(m) => m,
        other => other.to_string(),
    }
}

/// Longest staging file name we will build, in bytes.
///
/// 255 is `NAME_MAX` on every filesystem sn realistically writes to (ext4,
/// APFS, XFS, NTFS). The staging name is strictly longer than the destination's
/// own, so without a cap a destination that the OS accepts can still fail
/// `create` with ENAMETOOLONG — a download that works without staging must not
/// stop working because of it.
const MAX_STAGING_NAME: usize = 255;

/// Name for the staging file: dot-prefixed (hidden from a casual `ls`), tied to
/// the destination, and unique per process+instant so two concurrent downloads
/// of the same attachment cannot corrupt each other's staging file.
///
/// The destination's own name is only a readability affordance, so it is the
/// part that gets truncated when the whole thing would exceed
/// [`MAX_STAGING_NAME`]; uniqueness lives entirely in the pid+nanos suffix and
/// survives intact.
fn temp_file_name(dest: &Path) -> String {
    let base = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let suffix = format!(".sn{}-{nanos}.part", std::process::id());
    // 1 for the leading dot.
    let budget = MAX_STAGING_NAME.saturating_sub(suffix.len() + 1);
    format!(".{}{suffix}", truncate_bytes(base, budget))
}

/// Longest prefix of `s` that fits in `max` bytes without splitting a character.
fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Unlink the staging file if the user interrupts the download.
///
/// Without this, Ctrl-C on a large transfer leaves a hidden `.part` file in the
/// caller's directory that nothing ever reaps — and because the name carries a
/// fresh timestamp, every retry strands another one. Exits 130 (128 + SIGINT),
/// the conventional signal-death code: an aborted transfer is not a success and
/// must not be reported as one.
///
/// `set_handler` fails if a handler is already installed; that only happens if
/// some other command in this process claimed SIGINT first, in which case its
/// handling wins and losing the unlink is the lesser harm than refusing to
/// download.
fn reap_staging_on_sigint(temp_path: &Path) {
    let doomed = temp_path.to_path_buf();
    let _ = ctrlc::set_handler(move || {
        let _ = fs::remove_file(&doomed);
        std::process::exit(130);
    });
}

pub fn delete(global: &GlobalFlags, args: AttachmentDeleteArgs) -> Result<()> {
    confirm_delete(args.yes, &format!("attachment {}", args.sys_id))?;
    let client = connect(global)?;
    let path = format!("/api/now/attachment/{}", args.sys_id);
    client.delete(&path, &[])?;
    Ok(())
}

fn mime_from_extension(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("csv") => "text/csv",
        Some("txt") | Some("log") => "text/plain",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("zip") => "application/zip",
        Some("gz") | Some("gzip") => "application/gzip",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::{temp_file_name, truncate_bytes, MAX_STAGING_NAME};
    use std::path::Path;

    #[test]
    fn staging_name_stays_within_name_max() {
        // A destination name the OS accepts must not become one it rejects: the
        // staging name adds a pid+nanos suffix, so a long-but-legal destination
        // is exactly where ENAMETOOLONG appears.
        for len in [1usize, 200, 220, 240, 255] {
            let dest = format!("{}.bin", "a".repeat(len));
            let name = temp_file_name(Path::new(&dest));
            assert!(
                name.len() <= MAX_STAGING_NAME,
                "staging name for a {len}-char destination is {} bytes: {name}",
                name.len()
            );
            assert!(name.starts_with(".a"), "lost the readable prefix: {name}");
            assert!(name.ends_with(".part"), "lost the .part suffix: {name}");
        }
    }

    #[test]
    fn staging_name_keeps_the_unique_suffix_when_truncating() {
        // Uniqueness lives in pid+nanos; truncation must eat the readable base,
        // never the part that keeps concurrent downloads apart.
        let dest = "x".repeat(240);
        let a = temp_file_name(Path::new(&dest));
        // Separate the two calls by more than any platform's clock granularity,
        // so a collision means the timestamp was truncated away, not that the
        // clock failed to tick.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = temp_file_name(Path::new(&dest));
        assert_ne!(a, b, "two staging names collided: {a}");
        assert!(a.contains(&format!(".sn{}-", std::process::id())));
    }

    #[test]
    fn truncation_does_not_split_a_character() {
        // "é" is two bytes: a naive byte slice at 3 would panic.
        assert_eq!(truncate_bytes("aéb", 3), "aé");
        assert_eq!(truncate_bytes("aéb", 2), "a");
        assert_eq!(truncate_bytes("abc", 10), "abc");
        assert_eq!(truncate_bytes("abc", 0), "");
    }
}
