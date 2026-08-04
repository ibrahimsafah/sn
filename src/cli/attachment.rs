use crate::cli::table::{build_client, build_profile, confirm_delete, unwrap_or_raw};
use crate::cli::GlobalFlags;
use crate::error::{Error, Result};
use clap::Subcommand;
use std::io::{self, Write};
use std::path::Path;

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
    let (bytes, _ct) = client.download_file(&path)?;
    if let Some(out_path) = args.out {
        std::fs::write(&out_path, &bytes)
            .map_err(|e| Error::Usage(format!("write {out_path}: {e}")))?;
        let meta = serde_json::json!({
            "path": out_path,
            "size": bytes.len()
        });
        crate::cli::table::write_response(global, &meta)
    } else {
        io::stdout()
            .lock()
            .write_all(&bytes)
            .map_err(crate::output::map_stdout_err)?;
        Ok(())
    }
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
