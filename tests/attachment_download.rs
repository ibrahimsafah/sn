//! `sn attachment download` streams its body: the process must not buffer the
//! whole attachment, the destination path must never hold a truncated file, and
//! a transfer that is slow but alive must outlive the request timeout that
//! (correctly) still bounds every other call.

mod common;

use common::{ProfileSpec, sn_cmd, write_profiles};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

const SYS_ID: &str = "a1b2c3";

fn attachment_path() -> String {
    format!("/api/now/attachment/{SYS_ID}/file")
}

/// A payload big enough that a 64 KiB streaming buffer must cycle many times
/// over it, with position-dependent bytes so a lost or duplicated chunk shows up
/// as a content mismatch rather than only a length mismatch.
fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Names of the entries in `dir`, sorted.
fn dir_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Serve exactly one HTTP request on a fresh loopback port, handing the accepted
/// socket to `respond` once the request head has been consumed. Returns the
/// instance base URL to point a profile at.
///
/// Raw TCP rather than wiremock because every timeout property under test is
/// about *when bytes arrive within one body*, which a mock that returns a
/// complete canned response cannot express.
fn serve_raw<F>(respond: F) -> String
where
    F: FnOnce(&mut std::net::TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        // Drain the request head; the body is empty for a GET.
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match sock.read(&mut byte) {
                Ok(0) | Err(_) => return,
                Ok(_) => head.push(byte[0]),
            }
        }
        respond(&mut sock);
    });
    format!("http://127.0.0.1:{port}")
}

#[tokio::test(flavor = "current_thread")]
async fn large_body_lands_byte_exact_at_the_destination() {
    // 4 MiB is 64 times the streaming buffer: it cannot be served by accident
    // from a single read, so a correct file proves the loop, not luck.
    let body = payload(4 * 1024 * 1024);
    let expected = body.clone();
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(attachment_path()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body)
                .insert_header("Content-Type", "application/octet-stream"),
        )
        .mount(&server)
        .await;
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server.uri(),
            username: "u",
            password: "p",
        }],
    );

    tokio::task::spawn_blocking(move || {
        let dest_dir = tempfile::tempdir().unwrap();
        let dest = dest_dir.path().join("big.bin");
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args([
                "--compact",
                "attachment",
                "download",
                SYS_ID,
                "--out",
                dest.to_str().unwrap(),
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["size"], expected.len());
        assert_eq!(v["path"], dest.to_str().unwrap());

        let written = std::fs::read(&dest).unwrap();
        assert_eq!(written.len(), expected.len());
        assert!(
            written == expected,
            "downloaded bytes differ from the source"
        );

        // The staging file must not survive a successful download.
        assert_eq!(dir_entries(dest_dir.path()), vec!["big.bin".to_string()]);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn body_streams_to_stdout_when_no_out_is_given() {
    let body = payload(300 * 1024);
    let expected = body.clone();
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(attachment_path()))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(&server)
        .await;
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server.uri(),
            username: "u",
            password: "p",
        }],
    );

    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["attachment", "download", SYS_ID])
            .assert()
            .success();
        assert_eq!(out.get_output().stdout, expected);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn http_error_still_reports_the_api_envelope() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(attachment_path()))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": {"message": "No Record found", "detail": "gone"},
            "status": "failure"
        })))
        .mount(&server)
        .await;
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server.uri(),
            username: "u",
            password: "p",
        }],
    );

    tokio::task::spawn_blocking(move || {
        let dest_dir = tempfile::tempdir().unwrap();
        let dest = dest_dir.path().join("missing.bin");
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args([
                "attachment",
                "download",
                SYS_ID,
                "--out",
                dest.to_str().unwrap(),
            ])
            .assert()
            .code(2);
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
        assert_eq!(v["error"]["message"], "No Record found");
        assert_eq!(v["error"]["status_code"], 404);
        // A failed request must not leave a staging file behind either.
        assert!(dir_entries(dest_dir.path()).is_empty());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn truncated_stream_leaves_no_file_at_the_destination() {
    // Promise a megabyte, deliver 8 KiB, hang up. This is the silent-corruption
    // case: before staging+rename the destination would hold a short file that
    // looks exactly like a good one.
    let instance = serve_raw(|sock| {
        let head = "HTTP/1.1 200 OK\r\n\
                    Content-Type: application/octet-stream\r\n\
                    Content-Length: 1048576\r\n\r\n";
        sock.write_all(head.as_bytes()).unwrap();
        sock.write_all(&vec![7u8; 8 * 1024]).unwrap();
        sock.flush().unwrap();
        let _ = sock.shutdown(std::net::Shutdown::Both);
    });
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &instance,
            username: "u",
            password: "p",
        }],
    );

    tokio::task::spawn_blocking(move || {
        let dest_dir = tempfile::tempdir().unwrap();
        let dest = dest_dir.path().join("truncated.bin");
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args([
                "attachment",
                "download",
                SYS_ID,
                "--out",
                dest.to_str().unwrap(),
            ])
            .assert()
            .code(3);
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
        let msg = v["error"]["message"].as_str().unwrap();
        assert!(
            msg.contains("not written"),
            "error must say the destination was left alone: {msg}"
        );
        // `Error::Transport`'s Display prepends "transport error: ", but the
        // envelope emits the bare message — so interpolating an Error into a new
        // Transport double-prints the variant name, a prefix no other sn command
        // emits.
        assert!(
            !msg.contains("transport error"),
            "envelope message carries a redundant variant prefix: {msg}"
        );
        assert!(!dest.exists(), "truncated file left at the destination");
        assert!(
            dir_entries(dest_dir.path()).is_empty(),
            "staging file survived: {:?}",
            dir_entries(dest_dir.path())
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn failed_download_leaves_an_existing_destination_untouched() {
    // The property staging exists for, and the one `!dest.exists()` cannot
    // check: writing straight to the destination and unlinking it on failure
    // *also* passes that assertion, while destroying a file the caller already
    // had. A failed download must be a no-op at the destination, not a delete.
    let instance = serve_raw(|sock| {
        let head = "HTTP/1.1 200 OK\r\n\
                    Content-Type: application/octet-stream\r\n\
                    Content-Length: 1048576\r\n\r\n";
        sock.write_all(head.as_bytes()).unwrap();
        sock.write_all(&vec![7u8; 8 * 1024]).unwrap();
        sock.flush().unwrap();
        let _ = sock.shutdown(std::net::Shutdown::Both);
    });
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &instance,
            username: "u",
            password: "p",
        }],
    );

    tokio::task::spawn_blocking(move || {
        const PRECIOUS: &[u8] = b"PRECIOUS EXISTING CONTENT\n";
        let dest_dir = tempfile::tempdir().unwrap();
        let dest = dest_dir.path().join("keep.bin");
        std::fs::write(&dest, PRECIOUS).unwrap();

        let mut cmd = sn_cmd(tmp.path());
        cmd.args([
            "attachment",
            "download",
            SYS_ID,
            "--out",
            dest.to_str().unwrap(),
        ])
        .assert()
        .code(3);

        assert_eq!(
            std::fs::read(&dest).unwrap(),
            PRECIOUS,
            "a failed download overwrote or deleted the pre-existing destination"
        );
        // ...and it left no staging file beside it either.
        assert_eq!(dir_entries(dest_dir.path()), vec!["keep.bin".to_string()]);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn a_destination_name_that_fills_name_max_still_downloads() {
    // The staging name is longer than the destination's, so a name the
    // filesystem accepts can still blow past NAME_MAX once ~35 characters of
    // pid+timestamp are appended. 240 characters is legal on every filesystem
    // sn targets and must stay downloadable.
    let body = payload(4096);
    let expected = body.clone();
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(attachment_path()))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(&server)
        .await;
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server.uri(),
            username: "u",
            password: "p",
        }],
    );

    tokio::task::spawn_blocking(move || {
        let long_name = format!("{}.bin", "a".repeat(236));
        assert_eq!(long_name.len(), 240);
        let dest_dir = tempfile::tempdir().unwrap();
        let dest = dest_dir.path().join(&long_name);
        let mut cmd = sn_cmd(tmp.path());
        cmd.args([
            "attachment",
            "download",
            SYS_ID,
            "--out",
            dest.to_str().unwrap(),
        ])
        .assert()
        .success();
        assert_eq!(std::fs::read(&dest).unwrap(), expected);
        assert_eq!(dir_entries(dest_dir.path()), vec![long_name]);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn truncated_stream_to_stdout_exits_nonzero() {
    // Bytes already on the pipe cannot be recalled, so the exit code and the
    // envelope are the only signal that stdout is incomplete.
    let instance = serve_raw(|sock| {
        let head = "HTTP/1.1 200 OK\r\n\
                    Content-Type: application/octet-stream\r\n\
                    Content-Length: 1048576\r\n\r\n";
        sock.write_all(head.as_bytes()).unwrap();
        sock.write_all(&vec![7u8; 8 * 1024]).unwrap();
        sock.flush().unwrap();
        let _ = sock.shutdown(std::net::Shutdown::Both);
    });
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &instance,
            username: "u",
            password: "p",
        }],
    );

    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["attachment", "download", SYS_ID])
            .assert()
            .code(3);
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
        let msg = v["error"]["message"].as_str().unwrap();
        assert!(
            msg.contains("written to stdout"),
            "error must name the truncated stdout output: {msg}"
        );
        assert!(
            !msg.contains("transport error"),
            "envelope message carries a redundant variant prefix: {msg}"
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn slow_but_alive_transfer_outlives_the_request_timeout() {
    // Five chunks, 800ms apart: ~4s of wall clock against a 3s timeout. Under
    // the old `.bytes()` read the 3s cap covered the whole body and this died;
    // read-at-a-time makes the same cap a per-chunk idle timeout, so a healthy
    // slow transfer completes.
    //
    // What the gap and the timeout must satisfy is `gap < timeout < gap *
    // chunks`, and the *absolute* slack is what a loaded CI runner eats: a 300ms
    // gap under a 1s cap tolerated only a 700ms scheduling stall before failing
    // spuriously. 800ms under 3s tolerates 2.2s for the same property.
    let chunks = 5;
    let instance = serve_raw(move |sock| {
        let head = "HTTP/1.1 200 OK\r\n\
                    Content-Type: application/octet-stream\r\n\
                    Transfer-Encoding: chunked\r\n\r\n";
        sock.write_all(head.as_bytes()).unwrap();
        sock.flush().unwrap();
        for i in 0..chunks {
            let piece = format!("piece{i};");
            sock.write_all(format!("{:x}\r\n{piece}\r\n", piece.len()).as_bytes())
                .unwrap();
            sock.flush().unwrap();
            std::thread::sleep(Duration::from_millis(800));
        }
        sock.write_all(b"0\r\n\r\n").unwrap();
        sock.flush().unwrap();
    });
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &instance,
            username: "u",
            password: "p",
        }],
    );

    tokio::task::spawn_blocking(move || {
        let started = std::time::Instant::now();
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["--timeout", "3", "attachment", "download", SYS_ID])
            .assert()
            .success();
        assert!(
            started.elapsed() > Duration::from_secs(3),
            "the transfer finished too fast to have crossed the timeout"
        );
        assert_eq!(
            out.get_output().stdout,
            b"piece0;piece1;piece2;piece3;piece4;"
        );
    })
    .await
    .unwrap();
}

/// Ctrl-C during a download must not strand the hidden staging file in the
/// caller's directory. Nothing reaps it, and because the name embeds a fresh
/// timestamp, every retry of an interrupted download leaves another one.
///
/// Unix-only: there is no portable way to deliver SIGINT to a child on Windows.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn interrupting_a_download_removes_the_staging_file() {
    // Headers plus one chunk, then hold the socket open: the client is parked in
    // a read with the staging file created, which is exactly the window a Ctrl-C
    // lands in for a large download.
    let instance = serve_raw(|sock| {
        let head = "HTTP/1.1 200 OK\r\n\
                    Content-Type: application/octet-stream\r\n\
                    Content-Length: 1048576\r\n\r\n";
        sock.write_all(head.as_bytes()).unwrap();
        sock.write_all(&vec![7u8; 4096]).unwrap();
        sock.flush().unwrap();
        std::thread::sleep(Duration::from_secs(60));
    });
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &instance,
            username: "u",
            password: "p",
        }],
    );

    tokio::task::spawn_blocking(move || {
        let dest_dir = tempfile::tempdir().unwrap();
        let dest = dest_dir.path().join("interrupted.bin");
        let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("sn"))
            .env("SN_CONFIG_DIR", tmp.path())
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("ALL_PROXY")
            .args([
                // No idle timeout worth waiting for: the signal is the exit path
                // under test.
                "--timeout",
                "120",
                "attachment",
                "download",
                SYS_ID,
                "--out",
                dest.to_str().unwrap(),
            ])
            .spawn()
            .unwrap();

        // Wait for the staging file to appear — before it exists there is
        // nothing to leak and the test would prove nothing.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while dir_entries(dest_dir.path()).is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "download never created a staging file"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let staged = dir_entries(dest_dir.path());
        assert_eq!(staged.len(), 1, "unexpected staging state: {staged:?}");
        assert!(
            staged[0].starts_with('.') && staged[0].ends_with(".part"),
            "expected a hidden .part staging file, got {staged:?}"
        );

        let status = std::process::Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success(), "could not signal the child");
        let done = child.wait().unwrap();

        assert!(!dest.exists(), "an interrupted download wrote the file");
        assert!(
            dir_entries(dest_dir.path()).is_empty(),
            "Ctrl-C stranded a staging file: {:?}",
            dir_entries(dest_dir.path())
        );
        // 128 + SIGINT: an aborted transfer is not a success.
        assert_eq!(done.code(), Some(130), "unexpected exit status: {done:?}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn stalled_transfer_still_dies_at_the_idle_timeout() {
    // The other half of the bargain: "no total cap" must not mean "waits
    // forever". One chunk, then silence — the read timeout has to fire.
    let instance = serve_raw(|sock| {
        let head = "HTTP/1.1 200 OK\r\n\
                    Content-Type: application/octet-stream\r\n\
                    Transfer-Encoding: chunked\r\n\r\n";
        sock.write_all(head.as_bytes()).unwrap();
        sock.write_all(b"5\r\nalive\r\n").unwrap();
        sock.flush().unwrap();
        std::thread::sleep(Duration::from_secs(30));
    });
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &instance,
            username: "u",
            password: "p",
        }],
    );

    tokio::task::spawn_blocking(move || {
        let dest_dir = tempfile::tempdir().unwrap();
        let dest = dest_dir.path().join("stalled.bin");
        let started = std::time::Instant::now();
        let mut cmd = sn_cmd(tmp.path());
        cmd.args([
            "--timeout",
            "1",
            "attachment",
            "download",
            SYS_ID,
            "--out",
            dest.to_str().unwrap(),
        ])
        .assert()
        .code(3);
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "a stalled transfer must not hang until the server gives up"
        );
        assert!(!dest.exists());
        assert!(dir_entries(dest_dir.path()).is_empty());
    })
    .await
    .unwrap();
}
