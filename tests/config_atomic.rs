//! Concurrency and durability guarantees of the config persistence path.
//!
//! `sn`'s stated audience is LLM agents, which run parallel invocations as the
//! normal case, so "two processes wrote the config at once" is routine rather
//! than exotic. These tests pin the three properties that make that safe:
//! writes are atomic (a reader never sees half a file), read-modify-write is
//! serialized (no lost updates), and the write never touches the real config
//! directory — every test owns a private `TempDir`.
//!
//! Nothing here sets `SN_CONFIG_DIR` in-process; the library entry points take
//! an explicit path, and the one multi-process test passes the directory to the
//! child through its own environment. So these tests are parallel-safe.

use sn::config::{
    load_config_from, load_credentials_from, save_config_to, save_credentials_to, update_config_at,
    update_credentials_at, Config, Credentials, ProfileConfig, ProfileCredentials, TokenSet,
};
use std::path::Path;
use std::sync::{Arc, Barrier};

/// Number of concurrent writers. Enough to make an unlocked read-modify-write
/// lose an update essentially every run.
const WRITERS: usize = 12;

/// A long value per profile so the serialize + write is not so fast that the
/// interleaving window closes by accident — the test must exercise the lock,
/// not the scheduler's luck.
fn padding() -> String {
    "x".repeat(8_000)
}

fn tokens(tag: &str) -> TokenSet {
    TokenSet {
        access_token: format!("{tag}-{}", padding()),
        refresh_token: Some(format!("RT-{tag}")),
        expires_at: Some(1_700_000_000),
        token_type: Some("Bearer".into()),
    }
}

#[test]
fn concurrent_credential_writers_each_keep_their_profile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.toml");
    let barrier = Arc::new(Barrier::new(WRITERS));

    let handles: Vec<_> = (0..WRITERS)
        .map(|i| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                update_credentials_at(&path, |creds| {
                    creds.profiles.insert(
                        format!("p{i}"),
                        ProfileCredentials {
                            username: format!("u{i}"),
                            password: format!("pw{i}"),
                            oauth_tokens: Some(tokens(&format!("p{i}"))),
                            ..Default::default()
                        },
                    );
                    Ok(())
                })
                .unwrap();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let creds = load_credentials_from(&path).unwrap();
    for i in 0..WRITERS {
        let p = creds
            .profiles
            .get(&format!("p{i}"))
            .unwrap_or_else(|| panic!("profile p{i} was lost by a concurrent writer"));
        assert_eq!(p.username, format!("u{i}"));
        assert_eq!(p.password, format!("pw{i}"));
    }
    assert_eq!(creds.profiles.len(), WRITERS);
}

#[test]
fn concurrent_config_writers_each_keep_their_profile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let barrier = Arc::new(Barrier::new(WRITERS));

    let handles: Vec<_> = (0..WRITERS)
        .map(|i| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                update_config_at(&path, |cfg| {
                    cfg.profiles.insert(
                        format!("p{i}"),
                        ProfileConfig {
                            instance: format!("host{i}.example.com"),
                            ca_cert: Some(padding()),
                            ..Default::default()
                        },
                    );
                    Ok(())
                })
                .unwrap();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let cfg = load_config_from(&path).unwrap();
    assert_eq!(cfg.profiles.len(), WRITERS);
    for i in 0..WRITERS {
        assert_eq!(
            cfg.profiles[&format!("p{i}")].instance,
            format!("host{i}.example.com")
        );
    }
}

#[test]
fn a_reader_never_observes_a_half_written_file() {
    // `fs::write` truncates in place, so a concurrent reader can catch the file
    // empty or cut off mid-table and fail to parse. Replacement-by-rename makes
    // every read land on a complete file — the old one or the new one, never a
    // seam between them.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.toml");

    // Big enough (~1 MB) that a truncate-in-place write cannot finish inside one
    // syscall, which is what gives a racing reader something torn to see.
    const ROWS: usize = 128;
    let mut seed = Credentials::default();
    for i in 0..ROWS {
        seed.profiles.insert(
            format!("p{i}"),
            ProfileCredentials {
                username: format!("u{i}"),
                password: padding(),
                ..Default::default()
            },
        );
    }
    save_credentials_to(&path, &seed).unwrap();

    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let path = path.clone();
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let mut reads = 0usize;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                let creds = load_credentials_from(&path)
                    .expect("credentials.toml was observed torn mid-write");
                assert_eq!(
                    creds.profiles.len(),
                    ROWS,
                    "observed a partially-populated credentials file"
                );
                reads += 1;
            }
            reads
        })
    };

    for round in 0..60 {
        let mut next = seed.clone();
        next.profiles.get_mut("p0").unwrap().username = format!("round{round}");
        save_credentials_to(&path, &next).unwrap();
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let reads = reader.join().unwrap();
    assert!(reads > 0, "reader never got a chance to run");
}

#[test]
fn separate_processes_do_not_clobber_each_other() {
    // The in-process tests share a thread-local re-entrancy record; only separate
    // processes prove the lock is really held in the filesystem. `sn auth logout`
    // is the smallest command that does a full read-modify-write of
    // credentials.toml and needs no network, so N of them racing on one config
    // directory is the honest cross-process test: each must clear exactly its own
    // tokens and leave every other profile's secret intact.
    let dir = tempfile::tempdir().unwrap();
    seed_oauth_profiles(dir.path(), WRITERS);

    let bin = assert_cmd::cargo::cargo_bin("sn");
    let children: Vec<_> = (0..WRITERS)
        .map(|i| {
            let mut cmd = std::process::Command::new(&bin);
            cmd.arg("auth")
                .arg("logout")
                .arg("--profile")
                .arg(format!("p{i}"))
                .env("SN_CONFIG_DIR", dir.path())
                .env_remove("XDG_CONFIG_HOME")
                .stdout(std::process::Stdio::null());
            cmd.spawn().expect("spawn sn auth logout")
        })
        .collect();
    for mut child in children {
        let status = child.wait().unwrap();
        assert!(status.success(), "sn auth logout failed: {status:?}");
    }

    let creds = load_credentials_from(&dir.path().join("credentials.toml")).unwrap();
    assert_eq!(
        creds.profiles.len(),
        WRITERS,
        "a concurrent logout dropped profiles"
    );
    for i in 0..WRITERS {
        let p = &creds.profiles[&format!("p{i}")];
        assert!(
            p.oauth_tokens.is_none(),
            "profile p{i}'s logout was lost to a concurrent writer"
        );
        assert_eq!(
            p.client_secret.as_deref(),
            Some("shh"),
            "profile p{i} lost its client secret"
        );
    }
}

/// Write `count` OAuth profiles, each with a cached (padded) token set, into a
/// config directory rooted at `dir`.
fn seed_oauth_profiles(dir: &Path, count: usize) {
    let mut cfg = Config::default();
    let mut creds = Credentials::default();
    for i in 0..count {
        cfg.profiles.insert(
            format!("p{i}"),
            ProfileConfig {
                instance: format!("host{i}.example.com"),
                ..Default::default()
            },
        );
        creds.profiles.insert(
            format!("p{i}"),
            ProfileCredentials {
                client_secret: Some("shh".into()),
                oauth_tokens: Some(tokens(&format!("p{i}"))),
                ..Default::default()
            },
        );
    }
    save_config_to(&dir.join("config.toml"), &cfg).unwrap();
    save_credentials_to(&dir.join("credentials.toml"), &creds).unwrap();
}
