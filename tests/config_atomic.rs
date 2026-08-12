//! Concurrency and durability guarantees of the config persistence path.
//!
//! `sn`'s stated audience is LLM agents, which run parallel invocations as the
//! normal case, so "two processes wrote the config at once" is routine rather
//! than exotic. These tests pin the properties that make that safe: writes are
//! atomic (replacement by rename, never truncate-in-place), read-modify-write
//! is serialized (no lost updates), a rollback undoes only its own write, a
//! token refresh is single-flight, and the write never touches the real config
//! directory — every test owns a private `TempDir`.
//!
//! Nothing here sets `SN_CONFIG_DIR` in-process; the library entry points take
//! an explicit path, and the multi-process tests pass the directory to their
//! children through each child's own environment. So these tests are
//! parallel-safe.

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

/// Atomicity is a property of *how* the file is replaced, and the only honest
/// way to test it is to observe the mechanism rather than to race it.
///
/// Racing it does not work: an earlier version of this test spun a reader
/// thread parsing the file while a writer rewrote it 60 times, and it passed
/// just as happily with `fs::write` (truncate in place) as with the atomic
/// path, because the torn window is a handful of microseconds wide and the
/// reader has to be inside `read_to_string` for exactly that instant. A test
/// that cannot fail is worse than no test.
///
/// What is deterministic is that `rename(2)` gives the destination a **new
/// inode**, while truncate-in-place keeps the old one. So: hold an open handle
/// to the file across a write.
///
/// * Under replacement-by-rename the handle keeps pointing at the old, now
///   unlinked inode — it still reads the old contents, and its inode number
///   differs from the one the path now names. (Holding it open is also what
///   makes the inode comparison sound: the old inode cannot be recycled for
///   the new file while a descriptor still references it.)
/// * Under `fs::write` the handle and the path are the same inode, and the
///   handle sees the new contents. A reader that had the file open — or, in the
///   real failure, a reader `read`ing it mid-write — is looking at bytes from
///   two different versions.
///
/// Unix-only: this reads `st_ino`, and on Windows renaming over a file that
/// another handle has open is not permitted in the first place.
#[cfg(unix)]
#[test]
fn a_write_replaces_the_file_instead_of_truncating_it_in_place() {
    use std::io::{Read, Seek};
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.toml");

    // Big enough (~1 MB) that a truncate-in-place write is many syscalls wide —
    // the window a racing reader would have to hit.
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

    for round in 0..5 {
        // A reader that opened the file just before the write lands.
        let mut open_before = std::fs::File::open(&path).unwrap();
        let ino_before = open_before.metadata().unwrap().ino();
        let mut contents_before = String::new();
        open_before.read_to_string(&mut contents_before).unwrap();

        let mut next = seed.clone();
        next.profiles.get_mut("p0").unwrap().username = format!("round{round}");
        save_credentials_to(&path, &next).unwrap();

        let ino_after = std::fs::metadata(&path).unwrap().ino();
        assert_ne!(
            ino_before, ino_after,
            "round {round}: the destination kept its inode, so it was written in \
             place — a reader can see half of each version"
        );

        // The pre-existing handle must still see the version it opened, whole.
        open_before.rewind().unwrap();
        let mut contents_now = String::new();
        open_before.read_to_string(&mut contents_now).unwrap();
        assert_eq!(
            contents_now, contents_before,
            "round {round}: an already-open reader saw the file change underneath it"
        );
        assert!(
            !contents_now.contains(&format!("round{round}")),
            "round {round}: the new value leaked into a handle opened before the write"
        );

        // And the new version is complete and parses.
        let reread = load_credentials_from(&path).unwrap();
        assert_eq!(reread.profiles.len(), ROWS);
        assert_eq!(
            reread.profiles["p0"].username,
            format!("round{round}"),
            "round {round}: the write did not land"
        );
    }
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

#[test]
fn concurrent_profile_adds_all_survive() {
    // The issue's literal acceptance criterion, driven through the real CLI:
    // N `sn profile add` processes against one config directory, every one of
    // them still there afterwards. This is the path a user actually takes, and
    // it is the one that was broken — `save_profile` reads both files, mutates,
    // and writes them back, so locking only the two writes left the read
    // unprotected and every writer but one lost its profile while exiting 0 and
    // printing success JSON. `--no-verify` keeps it off the network.
    let dir = tempfile::tempdir().unwrap();
    let bin = assert_cmd::cargo::cargo_bin("sn");

    let children: Vec<_> = (0..WRITERS)
        .map(|i| {
            let mut cmd = std::process::Command::new(&bin);
            cmd.args([
                "profile",
                "add",
                &format!("p{i}"),
                "--instance",
                &format!("host{i}.example.com"),
                "--username",
                &format!("u{i}"),
                "--password",
                &format!("pw{i}"),
                "--no-verify",
            ])
            .env("SN_CONFIG_DIR", dir.path())
            .env_remove("XDG_CONFIG_HOME")
            .stdout(std::process::Stdio::null());
            cmd.spawn().expect("spawn sn profile add")
        })
        .collect();
    for mut child in children {
        assert!(
            child.wait().unwrap().success(),
            "sn profile add reported failure"
        );
    }

    let cfg = load_config_from(&dir.path().join("config.toml")).unwrap();
    let creds = load_credentials_from(&dir.path().join("credentials.toml")).unwrap();
    for i in 0..WRITERS {
        let name = format!("p{i}");
        assert_eq!(
            cfg.profiles
                .get(&name)
                .unwrap_or_else(|| panic!("{name} was dropped from config.toml"))
                .instance,
            format!("host{i}.example.com")
        );
        let c = creds
            .profiles
            .get(&name)
            .unwrap_or_else(|| panic!("{name} was dropped from credentials.toml"));
        assert_eq!(c.username, format!("u{i}"));
        assert_eq!(c.password, format!("pw{i}"));
    }
    assert_eq!(cfg.profiles.len(), WRITERS);
    assert_eq!(creds.profiles.len(), WRITERS);
}

#[test]
fn a_failed_verification_rolls_back_only_its_own_profile() {
    // `sn profile add` writes, then verifies over the network, then rolls back
    // if verification failed — and the rollback lands seconds after the write.
    // Restoring a whole-file snapshot there erases anything a sibling `sn`
    // wrote in between: one dead profile traded for someone else's live one.
    //
    // Staged deterministically. `stall` accepts the connection and then never
    // answers, so the verifying child is parked for its full `--timeout` while
    // a second child adds a profile and exits.
    let (port, _stall) = stalled_server();
    let dir = tempfile::tempdir().unwrap();
    let bin = assert_cmd::cargo::cargo_bin("sn");

    let mut doomed = std::process::Command::new(&bin)
        .args([
            "profile",
            "add",
            "doomed",
            "--instance",
            &format!("http://127.0.0.1:{port}"),
            "--username",
            "ud",
            "--password",
            "pd",
            "--timeout",
            "8",
        ])
        .env("SN_CONFIG_DIR", dir.path())
        .env_remove("XDG_CONFIG_HOME")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the verifying child");

    // Let it get past the write and into the stalled request.
    std::thread::sleep(std::time::Duration::from_millis(1_500));

    let keeper = std::process::Command::new(&bin)
        .args([
            "profile",
            "add",
            "keeper",
            "--instance",
            "keep.example.com",
            "--username",
            "uk",
            "--password",
            "pk",
            "--no-verify",
        ])
        .env("SN_CONFIG_DIR", dir.path())
        .env_remove("XDG_CONFIG_HOME")
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run the second add");
    assert!(keeper.success(), "the concurrent add should have succeeded");

    assert!(
        !doomed.wait().unwrap().success(),
        "verification against a server that never answers should fail"
    );

    let cfg = load_config_from(&dir.path().join("config.toml")).unwrap();
    let creds = load_credentials_from(&dir.path().join("credentials.toml")).unwrap();
    assert!(
        cfg.profiles.contains_key("keeper"),
        "the rollback erased a profile another process added while it was verifying"
    );
    assert_eq!(creds.profiles["keeper"].password, "pk");
    assert!(
        !cfg.profiles.contains_key("doomed"),
        "a profile that failed verification survived in config.toml"
    );
    assert!(
        !creds.profiles.contains_key("doomed"),
        "a profile that failed verification survived in credentials.toml"
    );
}

/// A loopback TCP server that accepts connections and then says nothing, so a
/// client blocks until its own timeout. Returns the port and a handle whose
/// lifetime keeps the server up; dropping it lets the accept loop die.
fn stalled_server() -> (u16, Arc<std::sync::atomic::AtomicBool>) {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = Arc::clone(&alive);
    std::thread::spawn(move || {
        // Accepted sockets are parked here rather than dropped: closing one
        // would hand the client a fast EOF instead of the stall we want.
        let mut parked = Vec::new();
        for stream in listener.incoming() {
            if !flag.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            match stream {
                Ok(s) => parked.push(s),
                Err(_) => return,
            }
        }
    });
    (port, alive)
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

/// A token refresh must be single-flight across processes, and on an instance
/// that rotates refresh tokens that is a correctness requirement rather than an
/// efficiency one.
///
/// The mock is the rotating IdP: the refresh token `RT0` is accepted exactly
/// once (returning a new access token *and* a new `RT1`), and every later
/// presentation of it — i.e. a second process that read the same cached
/// credentials — gets the 401 a spent grant deserves. `sn` maps that to exit 4,
/// "run `sn auth login`", moments after a perfectly valid token was minted.
///
/// Three invocations start together on one expired token. With the read →
/// refresh → write sequence held under the config lock and the staleness check
/// repeated inside it, exactly one of them talks to the token endpoint and the
/// other two find its result on disk. The 1.5s delay on the token response is
/// what makes the race deterministic: every child has read the stale token and
/// is committed to needing a refresh before the first one finishes.
#[tokio::test(flavor = "current_thread")]
async fn a_stale_token_is_refreshed_once_across_processes() {
    use wiremock::matchers::{body_string_contains, header, method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CHILDREN: usize = 3;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(wm_path("/oauth_token.do"))
        .and(body_string_contains("refresh_token=RT0"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(1_500))
                .set_body_json(serde_json::json!({
                    "access_token": "AT1",
                    "refresh_token": "RT1",
                    "expires_in": 1800,
                    "token_type": "Bearer"
                })),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    // Any further token request is a duplicate refresh: RT0 has been consumed.
    Mock::given(method("POST"))
        .and(wm_path("/oauth_token.do"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(serde_json::json!({"error": "invalid_grant"})),
        )
        .with_priority(9)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wm_path("/api/now/table/incident"))
        .and(header("authorization", "Bearer AT1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"result": []})))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    seed_expired_oauth_profile(dir.path(), &server.uri());

    let bin = assert_cmd::cargo::cargo_bin("sn");
    let root = dir.path().to_path_buf();
    let statuses = tokio::task::spawn_blocking(move || {
        let children: Vec<_> = (0..CHILDREN)
            .map(|_| {
                std::process::Command::new(&bin)
                    .args([
                        "--profile",
                        "oa",
                        "table",
                        "list",
                        "incident",
                        "--limit",
                        "1",
                    ])
                    .env("SN_CONFIG_DIR", &root)
                    .env_remove("XDG_CONFIG_HOME")
                    .env_remove("HTTP_PROXY")
                    .env_remove("HTTPS_PROXY")
                    .env_remove("ALL_PROXY")
                    .env_remove("SN_PROXY")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .expect("spawn sn table list")
            })
            .collect();
        children
            .into_iter()
            .map(|c| c.wait_with_output().unwrap())
            .collect::<Vec<_>>()
    })
    .await
    .unwrap();

    for (i, out) in statuses.iter().enumerate() {
        assert!(
            out.status.success(),
            "child {i} failed ({:?}) — a second process re-spent the rotated refresh \
             token instead of picking up the one already on disk: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // The rotated pair is what landed on disk, so the next invocation starts
    // from RT1 rather than the token the IdP already burned.
    let creds = load_credentials_from(&dir.path().join("credentials.toml")).unwrap();
    let saved = creds.profiles["oa"].oauth_tokens.as_ref().unwrap();
    assert_eq!(saved.access_token, "AT1");
    assert_eq!(saved.refresh_token.as_deref(), Some("RT1"));
}

/// Seed a config directory with one OAuth profile whose cached access token
/// expired ten seconds ago and whose refresh token is `RT0`.
fn seed_expired_oauth_profile(dir: &Path, instance: &str) {
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "default_profile = \"oa\"\n\n\
             [profiles.oa]\n\
             instance = \"{instance}\"\n\
             auth = \"oauth\"\n\n\
             [profiles.oa.oauth]\n\
             client_id = \"cid\"\n\
             grant = \"authorization_code\"\n\
             pkce = true\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("credentials.toml"),
        format!(
            "[profiles.oa]\n\n\
             [profiles.oa.oauth_tokens]\n\
             access_token = \"AT0\"\n\
             refresh_token = \"RT0\"\n\
             expires_at = {}\n\
             token_type = \"Bearer\"\n",
            sn::config::now_unix() - 10
        ),
    )
    .unwrap();
}

/// `sn profile remove` and `sn profile use` are read-modify-writes too, and the
/// finding named them alongside `add`. Racing all three against one directory
/// pins every one of them: a removal that gets lost leaves a profile (and its
/// password) on disk after the user deleted it, and a `use` that writes back a
/// config it read a moment ago silently deletes whatever landed in between.
#[test]
fn concurrent_removes_adds_and_uses_are_all_serialized() {
    const N: usize = 6;
    let dir = tempfile::tempdir().unwrap();
    let bin = assert_cmd::cargo::cargo_bin("sn");

    // Seed the profiles to be removed, plus one stable default.
    let mut cfg = Config {
        default_profile: Some("keep".into()),
        ..Default::default()
    };
    let mut creds = Credentials::default();
    for name in (0..N).map(|i| format!("p{i}")).chain(["keep".to_string()]) {
        cfg.profiles.insert(
            name.clone(),
            ProfileConfig {
                instance: format!("{name}.example.com"),
                ..Default::default()
            },
        );
        creds.profiles.insert(
            name.clone(),
            ProfileCredentials {
                username: name.clone(),
                password: padding(),
                ..Default::default()
            },
        );
    }
    save_credentials_to(&dir.path().join("credentials.toml"), &creds).unwrap();
    save_config_to(&dir.path().join("config.toml"), &cfg).unwrap();

    let spawn = |args: Vec<String>| {
        std::process::Command::new(&bin)
            .args(&args)
            .env("SN_CONFIG_DIR", dir.path())
            .env_remove("XDG_CONFIG_HOME")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sn")
    };

    let mut children = Vec::new();
    for i in 0..N {
        // `--yes` because these children have no terminal: `profile remove` is
        // gated like every other destructive command and refuses a
        // non-interactive stdin without it.
        children.push(spawn(vec![
            "profile".into(),
            "remove".into(),
            format!("p{i}"),
            "--yes".into(),
        ]));
        children.push(spawn(vec![
            "profile".into(),
            "add".into(),
            format!("n{i}"),
            "--instance".into(),
            format!("n{i}.example.com"),
            "--username".into(),
            format!("un{i}"),
            "--password".into(),
            format!("pn{i}"),
            "--no-verify".into(),
        ]));
        children.push(spawn(vec!["profile".into(), "use".into(), "keep".into()]));
    }
    for mut c in children {
        assert!(c.wait().unwrap().success(), "an sn profile command failed");
    }

    let cfg = load_config_from(&dir.path().join("config.toml")).unwrap();
    let creds = load_credentials_from(&dir.path().join("credentials.toml")).unwrap();
    assert_eq!(cfg.default_profile.as_deref(), Some("keep"));
    assert!(cfg.profiles.contains_key("keep"));
    for i in 0..N {
        assert!(
            !cfg.profiles.contains_key(&format!("p{i}")),
            "p{i}'s removal was lost to a concurrent writer"
        );
        assert!(
            !creds.profiles.contains_key(&format!("p{i}")),
            "p{i}'s stored password survived its removal"
        );
        assert!(
            cfg.profiles.contains_key(&format!("n{i}")),
            "n{i} was dropped by a concurrent remove or use"
        );
        assert_eq!(creds.profiles[&format!("n{i}")].password, format!("pn{i}"));
    }
}

/// `sn profile add` refuses to overwrite an existing profile without `--force`,
/// and that refusal has to hold under concurrency too. Checking only before the
/// prompt — which is where the check has to *start*, so a user is not asked for
/// a password that gets thrown away — leaves a window as wide as the prompt: N
/// invocations all read "the name is free" and all write. Exactly one may win.
#[test]
fn only_one_of_many_racing_adds_may_claim_a_name() {
    let dir = tempfile::tempdir().unwrap();
    let bin = assert_cmd::cargo::cargo_bin("sn");

    let children: Vec<_> = (0..WRITERS)
        .map(|i| {
            std::process::Command::new(&bin)
                .args([
                    "profile",
                    "add",
                    "contested",
                    "--instance",
                    &format!("host{i}.example.com"),
                    "--username",
                    &format!("u{i}"),
                    "--password",
                    &format!("pw{i}"),
                    "--no-verify",
                ])
                .env("SN_CONFIG_DIR", dir.path())
                .env_remove("XDG_CONFIG_HOME")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn sn profile add")
        })
        .collect();
    let winners = children
        .into_iter()
        .map(|mut c| c.wait().unwrap().success())
        .filter(|ok| *ok)
        .count();

    assert_eq!(
        winners, 1,
        "{winners} concurrent adds all believed the name was free"
    );

    // And the survivor's two files agree: whoever won wrote both halves.
    let cfg = load_config_from(&dir.path().join("config.toml")).unwrap();
    let creds = load_credentials_from(&dir.path().join("credentials.toml")).unwrap();
    let instance = cfg.profiles["contested"].instance.clone();
    let user = creds.profiles["contested"].username.clone();
    assert_eq!(
        instance,
        format!("{}.example.com", user.replace('u', "host"))
    );
}
