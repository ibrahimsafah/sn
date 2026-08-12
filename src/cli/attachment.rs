use crate::cli::table::{build_client, build_profile, confirm_delete, unwrap_or_raw};
use crate::cli::GlobalFlags;
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
    /// Maximum records returned. Maps to sysparm_limit.
    #[arg(long, alias = "sysparm-limit", alias = "limit", default_value_t = 100)]
    pub setlimit: u32,
    /// Starting offset for manual pagination.
    #[arg(long, alias = "sysparm-offset")]
    pub offset: Option<u32>,
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
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.query {
        query.push(("sysparm_query".into(), v));
    }
    query.push(("sysparm_limit".into(), args.setlimit.to_string()));
    if let Some(v) = args.offset {
        query.push(("sysparm_offset".into(), v.to_string()));
    }
    let resp = client.get("/api/now/attachment", &query)?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn get(global: &GlobalFlags, args: AttachmentGetArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/attachment/{}", args.sys_id);
    let resp = client.get(&path, &[])?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn upload(global: &GlobalFlags, args: AttachmentUploadArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
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
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn download(global: &GlobalFlags, args: AttachmentDownloadArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
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
    crate::cli::table::write_response(global, &meta)
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
        Err(DownloadError::Source(e)) => Err(Error::Transport(format!(
            "{e}; {} bytes of a truncated download were already written to stdout",
            download.bytes_written()
        ))),
    }
}

/// Turn a failed file download into an error that says the destination was left
/// untouched — the caller must not have to guess whether a retry is safe.
fn discarded(e: DownloadError, out_path: &str) -> Error {
    match e {
        DownloadError::Source(err) => Error::Transport(format!(
            "{err}; partial download discarded, {out_path} not written"
        )),
        DownloadError::Sink(err) => Error::Usage(format!(
            "write {out_path}: {err}; partial download discarded, {out_path} not written"
        )),
    }
}

/// Name for the staging file: dot-prefixed (hidden from a casual `ls`), tied to
/// the destination, and unique per process+instant so two concurrent downloads
/// of the same attachment cannot corrupt each other's staging file.
fn temp_file_name(dest: &Path) -> String {
    let base = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(".{base}.sn{}-{nanos}.part", std::process::id())
}

pub fn delete(global: &GlobalFlags, args: AttachmentDeleteArgs) -> Result<()> {
    confirm_delete(args.yes, &format!("attachment {}", args.sys_id))?;
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
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
