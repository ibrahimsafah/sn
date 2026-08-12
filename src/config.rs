use crate::error::{Error, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileConfig>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileConfig {
    pub instance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_proxy: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub insecure: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_cert: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_ca_cert: Option<String>,
    /// How requests authenticate. Defaults to `basic` so pre-OAuth profiles
    /// deserialize unchanged.
    #[serde(default, skip_serializing_if = "AuthMethod::is_basic")]
    pub auth: AuthMethod,
    /// Non-secret OAuth settings (the client secret + tokens live in
    /// `credentials.toml`). Present only when `auth = "oauth"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthConfig>,
}

/// Which credential mechanism a profile authenticates with.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lowercase")]
pub enum AuthMethod {
    /// HTTP Basic with a stored username/password.
    #[default]
    Basic,
    /// OAuth 2.0 bearer token (Authorization Code for SSO, or Client Credentials).
    Oauth,
}

impl AuthMethod {
    pub fn is_basic(&self) -> bool {
        matches!(self, AuthMethod::Basic)
    }
}

/// OAuth 2.0 grant type. `authorization_code` is the interactive (SSO) flow;
/// `client_credentials` is the service-to-service flow.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum OAuthGrant {
    #[default]
    AuthorizationCode,
    ClientCredentials,
}

impl OAuthGrant {
    fn is_default(&self) -> bool {
        matches!(self, OAuthGrant::AuthorizationCode)
    }
}

fn default_true() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

/// Non-secret OAuth configuration persisted in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthConfig {
    pub client_id: String,
    /// Loopback redirect registered in ServiceNow's Application Registry.
    /// Defaults to `http://localhost:8400/callback`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
    /// Authorization endpoint path. Defaults to `/oauth_auth.do`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_path: Option<String>,
    /// Token endpoint path. Defaults to `/oauth_token.do`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_path: Option<String>,
    #[serde(default, skip_serializing_if = "OAuthGrant::is_default")]
    pub grant: OAuthGrant,
    /// Use PKCE (S256) for the authorization-code flow. Defaults to true.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub pkce: bool,
}

/// An OAuth token set, persisted in `credentials.toml` (chmod 0600 on Unix).
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Absolute Unix-seconds expiry of the access token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
}

impl TokenSet {
    /// True when the access token is within `skew_secs` of (or past) expiry.
    /// Tokens with an unknown expiry are treated as valid until a 401 forces a
    /// re-login.
    pub fn is_expired(&self, skew_secs: u64) -> bool {
        match self.expires_at {
            Some(exp) => now_unix().saturating_add(skew_secs) >= exp,
            None => false,
        }
    }
}

/// Current wall-clock time in Unix seconds.
pub fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Default loopback redirect URI for the authorization-code flow.
pub fn default_redirect_uri() -> String {
    "http://localhost:8400/callback".to_string()
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Credentials {
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileCredentials>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileCredentials {
    /// Empty for OAuth profiles (which have no stored password).
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_password: Option<String>,
    /// OAuth client secret (confidential clients). Public/PKCE clients omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// Cached OAuth tokens, refreshed transparently when stale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_tokens: Option<TokenSet>,
}

/// Resolve the sn config directory.
///
/// If the `SN_CONFIG_DIR` environment variable is set to a non-empty value,
/// it is used as-is — this is the documented cross-platform override, and it
/// points **directly** at the directory containing `config.toml` and
/// `credentials.toml` (no `sn` subdirectory is appended). Otherwise the
/// platform-native location from `directories::ProjectDirs` is used.
pub fn config_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("SN_CONFIG_DIR") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    ProjectDirs::from("", "", "sn")
        .map(|pd| pd.config_dir().to_path_buf())
        .ok_or_else(|| Error::Config("cannot resolve home directory for config".into()))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn credentials_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("credentials.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_end_in_expected_filenames() {
        let cfg = config_path().unwrap();
        assert_eq!(cfg.file_name().unwrap(), "config.toml");
        let cred = credentials_path().unwrap();
        assert_eq!(cred.file_name().unwrap(), "credentials.toml");
    }

    #[test]
    fn profiles_roundtrip_via_toml() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "dev".into(),
            ProfileConfig {
                instance: "example.com".into(),
                ..Default::default()
            },
        );
        let cfg = Config {
            default_profile: Some("dev".into()),
            profiles,
        };
        let s = toml::to_string(&cfg).unwrap();
        let parsed: Config = toml::from_str(&s).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn credentials_roundtrip_via_toml() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "dev".into(),
            ProfileCredentials {
                username: "u".into(),
                password: "p".into(),
                ..Default::default()
            },
        );
        let cr = Credentials { profiles };
        let s = toml::to_string(&cr).unwrap();
        let parsed: Credentials = toml::from_str(&s).unwrap();
        assert_eq!(parsed, cr);
    }

    #[test]
    fn oauth_profile_config_roundtrips() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "sso".into(),
            ProfileConfig {
                instance: "acme.service-now.com".into(),
                auth: AuthMethod::Oauth,
                oauth: Some(OAuthConfig {
                    client_id: "abc".into(),
                    redirect_uri: Some("http://localhost:8400/callback".into()),
                    auth_path: None,
                    token_path: None,
                    grant: OAuthGrant::AuthorizationCode,
                    pkce: true,
                }),
                ..Default::default()
            },
        );
        let cfg = Config {
            default_profile: Some("sso".into()),
            profiles,
        };
        let s = toml::to_string(&cfg).unwrap();
        let parsed: Config = toml::from_str(&s).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn oauth_credentials_roundtrip_with_tokens() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "sso".into(),
            ProfileCredentials {
                client_secret: Some("shh".into()),
                oauth_tokens: Some(TokenSet {
                    access_token: "AT".into(),
                    refresh_token: Some("RT".into()),
                    expires_at: Some(1_700_000_000),
                    token_type: Some("Bearer".into()),
                }),
                ..Default::default()
            },
        );
        let cr = Credentials { profiles };
        let s = toml::to_string(&cr).unwrap();
        let parsed: Credentials = toml::from_str(&s).unwrap();
        assert_eq!(parsed, cr);
    }

    #[test]
    fn legacy_basic_config_without_auth_field_still_parses() {
        // A pre-OAuth config.toml has no `auth`/`oauth` keys; it must deserialize
        // as a Basic profile unchanged.
        let toml = r#"
default_profile = "dev"
[profiles.dev]
instance = "dev.example.com"
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        let p = cfg.profiles.get("dev").unwrap();
        assert_eq!(p.auth, AuthMethod::Basic);
        assert!(p.oauth.is_none());
    }

    #[test]
    fn basic_profile_omits_auth_and_oauth_keys_when_serialized() {
        // Backward-compatibility: serializing a plain Basic profile must not
        // emit `auth = "basic"` or an empty `[oauth]` table.
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "dev".into(),
            ProfileConfig {
                instance: "dev.example.com".into(),
                ..Default::default()
            },
        );
        let cfg = Config {
            default_profile: Some("dev".into()),
            profiles,
        };
        let s = toml::to_string(&cfg).unwrap();
        assert!(!s.contains("auth ="), "unexpected auth key:\n{s}");
        assert!(
            !s.contains("[profiles.dev.oauth]"),
            "unexpected oauth table:\n{s}"
        );
    }
}

// ---------------------------------------------------------------------------
// Persistence: locked read-modify-write + atomic, 0600-from-birth replacement.
//
// `sn`'s callers are agents, so N concurrent invocations against one config
// directory is the normal case rather than the edge. Three properties are load
// bearing here and none of them come from `fs::write`:
//
//   * `credentials.toml` must never exist, even for an instant, at anything but
//     0600. Write-then-chmod leaves a brand-new file at the umask default with
//     secrets already in it.
//   * A crash mid-write must leave the old file, not half of the new one.
//   * Two processes doing load → modify → save must not silently drop each
//     other's changes.
// ---------------------------------------------------------------------------

/// Advisory lockfile serializing writes to one config directory.
///
/// It is a **sidecar**, deliberately not `config.toml`/`credentials.toml`
/// themselves: those are replaced by `rename(2)`, so a lock taken on one of them
/// is a lock on an inode the next writer will never open — each writer would
/// hold "the lock" on a different inode and none would block. Only a file that
/// is created once and never renamed over has a stable identity to lock.
///
/// One lock covers the whole directory rather than one per file. The two files
/// are written as a pair (adding a profile touches both), and one lock lets a
/// writer hold the pair together for the whole read → modify → write — see
/// [`with_config_lock`], which is how `sn profile add` and friends stay atomic
/// as a pair. A single lock also means there is no lock ordering, and so no
/// deadlock.
///
/// **Readers take no lock**, and that is deliberate: every `sn` invocation
/// reads the config, and making each one contend with writers would trade a
/// rare lost update for a common stall. Instead, *write ordering* is what keeps
/// an unlocked reader honest. Readers load `config.toml` first and
/// `credentials.toml` second (`cli::table::build_profile`), and the one state
/// that hurts them is a config entry whose credentials have not landed —
/// `resolve_profile` then hands back `Auth::Basic("", "")` and the caller gets a
/// baffling 401. So writers go the opposite way round:
///
/// * **Adding** writes `credentials.toml`, then `config.toml`. A reader that
///   sees the new config entry read it *after* the credentials rename, so it is
///   guaranteed to see the credentials too — the interleaving that breaks is
///   arithmetically impossible, not merely unlikely. In between, the profile
///   simply reads as not existing yet.
/// * **Removing** writes `config.toml`, then `credentials.toml`, so a reader
///   that runs entirely inside the gap sees the profile already gone rather
///   than present-but-credential-less. This one is narrower rather than
///   airtight: a reader whose two loads straddle *both* renames can still
///   catch the bad pair. Closing that last window needs a shared read lock on
///   the reader side; the residue is a two-`read(2)` window around a delete of
///   the very profile being read, so it has not earned that cost.
const LOCK_FILE: &str = ".sn.lock";

/// How long a write waits for the lock before giving up. Every critical section
/// is a parse, a mutation and one small write, so reaching this bound means
/// another process is wedged — an agent gets a diagnosable error rather than an
/// invocation that never returns.
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

thread_local! {
    /// Config directories whose lock this thread already holds. `flock(2)` locks
    /// an open file *description*, so a second `open` + lock from the same
    /// thread contends with the first: nesting a locked write inside another one
    /// would deadlock the process against itself. Recording the held directories
    /// per thread makes that nesting a no-op while leaving contention between
    /// threads and processes fully intact — each of those opens its own
    /// description and so blocks as intended.
    static HELD_LOCKS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// RAII holder for the directory lock. Dropping it closes the file handle,
/// which is what releases the `flock`.
struct DirLock {
    dir: PathBuf,
    _file: Option<File>,
}

impl DirLock {
    fn acquire(dir: &Path) -> Result<Self> {
        Self::acquire_for(dir, LOCK_TIMEOUT)
    }

    fn acquire_for(dir: &Path, timeout: Duration) -> Result<Self> {
        fs::create_dir_all(dir)
            .map_err(|e| Error::Config(format!("mkdir {}: {e}", dir.display())))?;
        // Canonicalized (after the mkdir, so it can succeed) because the
        // re-entrancy record is keyed by path: two spellings of one directory —
        // `cfg` vs `./cfg`, or either side of a symlink — would otherwise look
        // like different locks, and the inner acquisition would block on the
        // outer one's own flock until the timeout.
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let held = HELD_LOCKS.with(|h| h.borrow().contains(&dir));
        let file = if held {
            None
        } else {
            Some(lock_dir(&dir, timeout)?)
        };
        HELD_LOCKS.with(|h| h.borrow_mut().push(dir.clone()));
        Ok(DirLock { dir, _file: file })
    }
}

impl Drop for DirLock {
    fn drop(&mut self) {
        // `try_with` because a guard could outlive TLS teardown at thread exit,
        // where `with` panics.
        let _ = HELD_LOCKS.try_with(|h| {
            let mut h = h.borrow_mut();
            if let Some(pos) = h.iter().rposition(|d| *d == self.dir) {
                h.remove(pos);
            }
        });
    }
}

fn lock_dir(dir: &Path, timeout: Duration) -> Result<File> {
    let path = dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| Error::Config(format!("open {}: {e}", path.display())))?;

    let deadline = Instant::now() + timeout;
    let mut backoff = Duration::from_millis(1);
    loop {
        // Spelled as a trait call, not `file.try_lock()`: Rust 1.89 stabilized an
        // inherent `File::try_lock` that would silently shadow this one, and the
        // crate's MSRV predates it.
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => return Ok(file),
            Err(fs4::TryLockError::WouldBlock) => {}
            Err(fs4::TryLockError::Error(e)) => {
                return Err(Error::Config(format!("lock {}: {e}", path.display())))
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::Config(format!(
                "timed out after {}s waiting for the config lock at {}; another sn process is holding it",
                timeout.as_secs(),
                path.display()
            )));
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(25));
    }
}

/// The directory whose lock guards `path` — its parent, or the working
/// directory for a bare filename.
fn lock_dir_of(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Run `f` holding the config directory's exclusive advisory lock.
///
/// This is the wrapper any load → modify → save sequence needs: the `save_*`
/// functions lock their own writes, but a lock taken only around the write does
/// nothing about two processes that each read the old file first. Re-entrant
/// within a thread, so the inner `save_*` call is free.
pub fn with_config_lock<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    with_config_lock_for(LOCK_TIMEOUT, f)
}

/// [`with_config_lock`] with a caller-chosen acquisition timeout.
///
/// The default [`LOCK_TIMEOUT`] is sized for the only thing writers normally do
/// under the lock: parse, mutate, write. One critical section is not that shape
/// — `oauth::ensure_access_token` holds the lock across the `/oauth_token.do`
/// round trip, because a refresh-token rotation makes a *duplicated* refresh
/// worse than a serialized one (the loser presents an already-consumed refresh
/// token and gets a 401). A waiter there has to out-wait a holder that is
/// blocked on the network, so it passes the HTTP timeout plus the usual slack;
/// with the default 10s every parallel invocation would instead fail with a
/// spurious lock timeout the moment the IdP got slow.
pub fn with_config_lock_for<T>(timeout: Duration, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let dir = config_dir()?;
    let _guard = DirLock::acquire_for(&dir, timeout)?;
    f()
}

/// Read `config.toml`, hand it to `f`, and write it back — all under one lock,
/// so a concurrent writer cannot land between the read and the write.
pub fn update_config_at<T>(path: &Path, f: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    let _guard = DirLock::acquire(lock_dir_of(path))?;
    let mut config = load_config_from(path)?;
    let out = f(&mut config)?;
    save_config_to(path, &config)?;
    Ok(out)
}

/// Read `credentials.toml`, hand it to `f`, and write it back under one lock.
/// See [`update_config_at`].
pub fn update_credentials_at<T>(
    path: &Path,
    f: impl FnOnce(&mut Credentials) -> Result<T>,
) -> Result<T> {
    let _guard = DirLock::acquire(lock_dir_of(path))?;
    let mut creds = load_credentials_from(path)?;
    let out = f(&mut creds)?;
    save_credentials_to(path, &creds)?;
    Ok(out)
}

/// A temp file in `dir`, created 0600 by `open(2)` itself.
///
/// The mode is passed to the open, never applied afterwards: create-then-chmod
/// is precisely the window this module exists to close.
fn new_temp_file(dir: &Path) -> Result<tempfile::NamedTempFile> {
    let mut builder = tempfile::Builder::new();
    // Leading dot + recognizable stem so anything a hard crash strands here is
    // identifiable rather than mistaken for a real config file.
    builder.prefix(".sn-tmp");
    #[cfg(unix)]
    builder.permissions(fs::Permissions::from_mode(0o600));
    builder
        .tempfile_in(dir)
        .map_err(|e| Error::Config(format!("create temp file in {}: {e}", dir.display())))
}

/// Replace `path`'s contents with `contents` in one step.
///
/// The bytes land in a temp file **in the same directory** — `rename(2)` is only
/// atomic within a single filesystem, so a temp file under `/tmp` would be a
/// copy, not a rename.
///
/// Crash-durability takes two fsyncs, and each covers a different half:
///
/// * `sync_all` on the temp file **before** the rename forces the contents out,
///   so the rename cannot publish a name pointing at blocks that were never
///   written. Without it the crash outcome would be "the old file, the new
///   file, or the new file full of zeroes".
/// * `fsync` on the containing **directory** after the rename forces the
///   directory entry out, so the rename itself cannot be lost. Without it the
///   *content* is safe but the rename is not durable, and a crash can roll the
///   whole write back to the old file — that is a legal outcome for us, but it
///   means "saved" was reported for a write that then vanished, which for a
///   password prompt is worth one extra fsync to avoid.
///
/// Directory fsync is Unix-only: on Windows a directory cannot be opened as a
/// file, and `MoveFileEx`'s metadata ordering is the filesystem's problem
/// rather than something a caller can force.
///
/// The replacement carries the temp file's 0600 mode onto the destination, which
/// also repairs a `credentials.toml` left 0644 by an older release.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let dir = lock_dir_of(path);
    fs::create_dir_all(dir).map_err(|e| Error::Config(format!("mkdir {}: {e}", dir.display())))?;
    let mut tmp = new_temp_file(dir)?;
    tmp.write_all(contents.as_bytes())
        .map_err(|e| Error::Config(format!("write {}: {e}", path.display())))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| Error::Config(format!("sync {}: {e}", path.display())))?;
    tmp.persist(path)
        .map_err(|e| Error::Config(format!("replace {}: {}", path.display(), e.error)))?;
    sync_dir(dir);
    Ok(())
}

/// `fsync` a directory so a rename into it is durable. Best-effort: a
/// filesystem that refuses (some network mounts return `EINVAL`) does not make
/// the write any less *atomic*, which is the property this module promises, so
/// failing the whole save over it would be worse than the durability it buys.
#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

pub fn load_config_from(path: &std::path::Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let s = fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("read {}: {e}", path.display())))?;
    toml::from_str(&s).map_err(|e| Error::Config(format!("parse {}: {e}", path.display())))
}

pub fn load_credentials_from(path: &std::path::Path) -> Result<Credentials> {
    if !path.exists() {
        return Ok(Credentials::default());
    }
    let s = fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("read {}: {e}", path.display())))?;
    toml::from_str(&s).map_err(|e| Error::Config(format!("parse {}: {e}", path.display())))
}

/// Persist `cfg`. Written 0600 like `credentials.toml`: it names the instances
/// and OAuth clients one user talks to, and nothing else needs to read it.
pub fn save_config_to(path: &std::path::Path, cfg: &Config) -> Result<()> {
    let s =
        toml::to_string_pretty(cfg).map_err(|e| Error::Config(format!("serialize config: {e}")))?;
    let _guard = DirLock::acquire(lock_dir_of(path))?;
    write_atomic(path, &s)
}

/// Persist `cr`. There is no chmod here on purpose — the file is born 0600 in
/// [`new_temp_file`] and keeps that mode across the rename, so the secrets are
/// never readable by anyone else, not even for the instant a write-then-chmod
/// would leave open.
pub fn save_credentials_to(path: &std::path::Path, cr: &Credentials) -> Result<()> {
    let s = toml::to_string_pretty(cr)
        .map_err(|e| Error::Config(format!("serialize credentials: {e}")))?;
    let _guard = DirLock::acquire(lock_dir_of(path))?;
    write_atomic(path, &s)
}

/// Resolved profile ready to make HTTP calls. Built by applying precedence:
/// CLI flag > env var > file value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub name: String,
    pub instance: String,
    pub username: String,
    pub password: String,
    pub proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub insecure: bool,
    pub ca_cert: Option<String>,
    pub proxy_ca_cert: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
    /// How this profile authenticates.
    pub auth_method: AuthMethod,
    /// OAuth state (config + cached tokens). `Some` iff `auth_method` is Oauth.
    pub oauth: Option<ResolvedOauth>,
}

/// Fully-resolved OAuth state for a single invocation: client config plus any
/// cached tokens. All endpoint/redirect defaults are already applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOauth {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub auth_path: String,
    pub token_path: String,
    pub grant: OAuthGrant,
    pub pkce: bool,
    pub tokens: Option<TokenSet>,
}

pub struct ProfileResolverInputs<'a> {
    pub cli_profile: Option<&'a str>,
    pub cli_proxy: Option<&'a str>,
    pub env_proxy: Option<&'a str>,
    pub cli_no_proxy: bool,
    pub env_no_proxy: Option<&'a str>,
    pub cli_insecure: bool,
    pub env_insecure: Option<&'a str>,
    pub cli_ca_cert: Option<&'a str>,
    pub env_ca_cert: Option<&'a str>,
    pub cli_proxy_ca_cert: Option<&'a str>,
    pub env_proxy_ca_cert: Option<&'a str>,
    pub config: &'a Config,
    pub credentials: &'a Credentials,
}

/// Resolve which profile name an invocation targets: the `--profile` flag
/// wins, then `default_profile` from config.toml. With neither there is no
/// implicit fallback — selection fails with a clear error instead of
/// inventing a profile nobody created.
pub fn resolve_profile_name(cli_profile: Option<&str>, config: &Config) -> Result<String> {
    cli_profile
        .map(ToString::to_string)
        .or_else(|| config.default_profile.clone())
        .ok_or_else(|| {
            Error::Config("no profile selected; pass --profile <name> or run `sn init`".into())
        })
}

pub fn resolve_profile(inputs: ProfileResolverInputs<'_>) -> Result<ResolvedProfile> {
    let name = resolve_profile_name(inputs.cli_profile, inputs.config)?;

    let profile_cfg = inputs.config.profiles.get(&name);
    let profile_cred = inputs.credentials.profiles.get(&name);

    // An absent profile and a profile with an empty/whitespace instance are the
    // same failure: without a real host there is nothing to talk to (an empty
    // instance would otherwise produce the scheme-only base URL "https://").
    let instance = profile_cfg
        .map(|p| p.instance.clone())
        .filter(|i| !i.trim().is_empty())
        .ok_or_else(|| {
            Error::Config(format!(
                "no instance configured for profile '{name}'; run `sn init`"
            ))
        })?;

    let auth_method = profile_cfg.map(|p| p.auth).unwrap_or_default();

    let username_opt = profile_cred.map(|p| p.username.clone());
    let password_opt = profile_cred.map(|p| p.password.clone());

    // Basic auth requires a username/password; OAuth profiles don't store them.
    let (username, password) = match auth_method {
        AuthMethod::Basic => (
            username_opt.ok_or_else(|| {
                Error::Config(format!(
                    "no username configured for profile '{name}'; run `sn init`"
                ))
            })?,
            password_opt.ok_or_else(|| {
                Error::Config(format!(
                    "no password configured for profile '{name}'; run `sn init`"
                ))
            })?,
        ),
        AuthMethod::Oauth => (
            username_opt.unwrap_or_default(),
            password_opt.unwrap_or_default(),
        ),
    };

    let oauth = match auth_method {
        AuthMethod::Basic => None,
        AuthMethod::Oauth => {
            let cfg = profile_cfg.and_then(|p| p.oauth.as_ref()).ok_or_else(|| {
                Error::Config(format!(
                    "profile '{name}' uses oauth but has no [oauth] config; run `sn init`"
                ))
            })?;
            Some(ResolvedOauth {
                client_id: cfg.client_id.clone(),
                client_secret: profile_cred.and_then(|c| c.client_secret.clone()),
                redirect_uri: cfg
                    .redirect_uri
                    .clone()
                    .unwrap_or_else(default_redirect_uri),
                auth_path: cfg
                    .auth_path
                    .clone()
                    .unwrap_or_else(|| "/oauth_auth.do".to_string()),
                token_path: cfg
                    .token_path
                    .clone()
                    .unwrap_or_else(|| "/oauth_token.do".to_string()),
                grant: cfg.grant,
                pkce: cfg.pkce,
                tokens: profile_cred.and_then(|c| c.oauth_tokens.clone()),
            })
        }
    };

    let proxy = if inputs.cli_no_proxy {
        None
    } else {
        inputs
            .cli_proxy
            .map(ToString::to_string)
            .or_else(|| inputs.env_proxy.map(ToString::to_string))
            .or_else(|| profile_cfg.and_then(|p| p.proxy.clone()))
    };

    let no_proxy = inputs
        .env_no_proxy
        .map(ToString::to_string)
        .or_else(|| profile_cfg.and_then(|p| p.no_proxy.clone()));

    let insecure = inputs.cli_insecure
        || inputs
            .env_insecure
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        || profile_cfg.map(|p| p.insecure).unwrap_or(false);

    let ca_cert = inputs
        .cli_ca_cert
        .map(ToString::to_string)
        .or_else(|| inputs.env_ca_cert.map(ToString::to_string))
        .or_else(|| profile_cfg.and_then(|p| p.ca_cert.clone()));

    let proxy_ca_cert = inputs
        .cli_proxy_ca_cert
        .map(ToString::to_string)
        .or_else(|| inputs.env_proxy_ca_cert.map(ToString::to_string))
        .or_else(|| profile_cfg.and_then(|p| p.proxy_ca_cert.clone()));

    let proxy_username = profile_cred.and_then(|p| p.proxy_username.clone());
    let proxy_password = profile_cred.and_then(|p| p.proxy_password.clone());

    Ok(ResolvedProfile {
        name,
        instance,
        username,
        password,
        proxy,
        no_proxy,
        insecure,
        ca_cert,
        proxy_ca_cert,
        proxy_username,
        proxy_password,
        auth_method,
        oauth,
    })
}

/// Persist a refreshed/new OAuth token set for `profile_name` into
/// `credentials.toml`, leaving every other field untouched.
///
/// Locked read-modify-write: parallel invocations of `sn` each refresh their own
/// profile, and an unlocked last-writer-wins would strand the losers' refresh
/// tokens at their pre-refresh values — dead, once the instance rotates them.
pub fn save_oauth_tokens(profile_name: &str, tokens: &TokenSet) -> Result<()> {
    update_credentials_at(&credentials_path()?, |creds| {
        creds
            .profiles
            .entry(profile_name.to_string())
            .or_default()
            .oauth_tokens = Some(tokens.clone());
        Ok(())
    })
}

/// Re-read `profile_name`'s cached OAuth tokens straight from disk.
///
/// A `ResolvedProfile`'s tokens are a snapshot taken when the invocation
/// started; this is how `oauth::ensure_access_token` finds out, from inside the
/// write lock, whether a sibling process has landed a fresher token since. A
/// missing file or profile is `None`, not an error — that is just "no token
/// cached".
pub fn load_oauth_tokens(profile_name: &str) -> Result<Option<TokenSet>> {
    let creds = load_credentials_from(&credentials_path()?)?;
    Ok(creds
        .profiles
        .get(profile_name)
        .and_then(|p| p.oauth_tokens.clone()))
}

/// Drop any cached OAuth tokens for `profile_name` (used by `sn auth logout`).
pub fn clear_oauth_tokens(profile_name: &str) -> Result<()> {
    update_credentials_at(&credentials_path()?, |creds| {
        if let Some(p) = creds.profiles.get_mut(profile_name) {
            p.oauth_tokens = None;
        }
        Ok(())
    })
}

#[cfg(test)]
mod resolution_tests {
    use super::*;

    fn sample_config() -> Config {
        let mut cfg = Config {
            default_profile: Some("dev".into()),
            ..Default::default()
        };
        cfg.profiles.insert(
            "dev".into(),
            ProfileConfig {
                instance: "dev.example.com".into(),
                ..Default::default()
            },
        );
        cfg.profiles.insert(
            "prod".into(),
            ProfileConfig {
                instance: "prod.example.com".into(),
                ..Default::default()
            },
        );
        cfg
    }

    fn sample_credentials() -> Credentials {
        let mut cr = Credentials::default();
        cr.profiles.insert(
            "dev".into(),
            ProfileCredentials {
                username: "dev-u".into(),
                password: "dev-p".into(),
                ..Default::default()
            },
        );
        cr.profiles.insert(
            "prod".into(),
            ProfileCredentials {
                username: "prod-u".into(),
                password: "prod-p".into(),
                ..Default::default()
            },
        );
        cr
    }

    fn base_inputs<'a>(cfg: &'a Config, cr: &'a Credentials) -> ProfileResolverInputs<'a> {
        ProfileResolverInputs {
            cli_profile: None,
            cli_proxy: None,
            env_proxy: None,
            cli_no_proxy: false,
            env_no_proxy: None,
            cli_insecure: false,
            env_insecure: None,
            cli_ca_cert: None,
            env_ca_cert: None,
            cli_proxy_ca_cert: None,
            env_proxy_ca_cert: None,
            config: cfg,
            credentials: cr,
        }
    }

    #[test]
    fn default_profile_when_none_specified() {
        let cfg = sample_config();
        let cr = sample_credentials();
        let r = resolve_profile(base_inputs(&cfg, &cr)).unwrap();
        assert_eq!(r.name, "dev");
        assert_eq!(r.instance, "dev.example.com");
    }

    #[test]
    fn cli_profile_wins_over_default() {
        let cfg = sample_config();
        let cr = sample_credentials();
        let r = resolve_profile(ProfileResolverInputs {
            cli_profile: Some("prod"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert_eq!(r.name, "prod");
        assert_eq!(r.instance, "prod.example.com");
    }

    #[test]
    fn no_profile_selected_errors_clearly() {
        // No --profile flag and no default_profile: selection must fail with
        // a clear error instead of falling back to a phantom "default".
        let cfg = Config::default();
        let cr = Credentials::default();
        let err = resolve_profile(base_inputs(&cfg, &cr)).unwrap_err();
        match err {
            Error::Config(msg) => assert!(
                msg.contains("no profile selected")
                    && msg.contains("--profile")
                    && msg.contains("sn init"),
                "unexpected error message: {msg}"
            ),
            other => panic!("expected Error::Config, got: {other:?}"),
        }
    }

    #[test]
    fn empty_instance_rejected() {
        // A stored profile whose instance is empty/whitespace must be rejected
        // instead of producing a scheme-only base URL like "https://".
        let mut cfg = sample_config();
        cfg.profiles.get_mut("dev").unwrap().instance = "   ".into();
        let cr = sample_credentials();
        let err = resolve_profile(base_inputs(&cfg, &cr)).unwrap_err();
        match err {
            Error::Config(msg) => assert!(
                msg.contains("no instance configured for profile 'dev'"),
                "unexpected error message: {msg}"
            ),
            other => panic!("expected Error::Config, got: {other:?}"),
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.toml");
        let cr_path = dir.path().join("credentials.toml");
        let cfg = sample_config();
        let cr = sample_credentials();
        save_config_to(&cfg_path, &cfg).unwrap();
        save_credentials_to(&cr_path, &cr).unwrap();
        assert_eq!(load_config_from(&cfg_path).unwrap(), cfg);
        assert_eq!(load_credentials_from(&cr_path).unwrap(), cr);
    }

    #[cfg(unix)]
    #[test]
    fn credentials_file_is_chmod_600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        save_credentials_to(&path, &sample_credentials()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn temp_file_is_born_0600_not_chmodded_later() {
        // The acceptance test for the permission window: the file the secrets are
        // actually written into must already be 0600 at open(2), because anything
        // applied afterwards leaves an interval where a umask-default 0644 file
        // holds a password. Asserted on the temp file itself, since by the time
        // credentials.toml exists the rename has already happened.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let tmp = new_temp_file(dir.path()).unwrap();
        let mode = tmp.as_file().metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "temp file created world/group-readable");
    }

    #[cfg(unix)]
    #[test]
    fn config_file_is_not_group_or_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        save_config_to(&path, &sample_config()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn rewrite_repairs_a_credentials_file_left_0644() {
        // Files left by an older release (or by a hand-edit) can be sitting at
        // 0644. The rename carries the temp file's mode onto the destination, so
        // the next write must fix them rather than preserve the exposure.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        fs::write(&path, "").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        save_credentials_to(&path, &sample_credentials()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn writes_leave_no_temp_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        save_config_to(&dir.path().join("config.toml"), &sample_config()).unwrap();
        save_credentials_to(&dir.path().join("credentials.toml"), &sample_credentials()).unwrap();
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec![".sn.lock", "config.toml", "credentials.toml"]);
    }

    #[test]
    fn nested_locks_on_one_directory_do_not_deadlock() {
        // `update_credentials_at` calls `save_credentials_to`, which locks too.
        // Without the per-thread re-entrancy record that inner acquisition would
        // block on the outer one's flock forever (well, until LOCK_TIMEOUT), and
        // every token refresh would hang.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        update_credentials_at(&path, |creds| {
            creds
                .profiles
                .insert("nested".into(), ProfileCredentials::default());
            Ok(())
        })
        .unwrap();
        assert!(load_credentials_from(&path)
            .unwrap()
            .profiles
            .contains_key("nested"));
    }

    #[test]
    fn two_spellings_of_one_directory_are_one_lock() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = DirLock::acquire(dir.path()).unwrap();
        // The same directory reached through a "." component. The re-entrancy
        // record is path-keyed, so without canonicalization this inner write
        // would sit on the outer guard's own flock until LOCK_TIMEOUT.
        let path = dir.path().join(".").join("credentials.toml");
        save_credentials_to(&path, &sample_credentials()).unwrap();
        assert!(dir.path().join("credentials.toml").exists());
    }

    #[test]
    fn update_helpers_see_and_keep_prior_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        save_config_to(&path, &sample_config()).unwrap();
        update_config_at(&path, |cfg| {
            cfg.default_profile = Some("prod".into());
            Ok(())
        })
        .unwrap();
        let loaded = load_config_from(&path).unwrap();
        assert_eq!(loaded.default_profile.as_deref(), Some("prod"));
        assert!(loaded.profiles.contains_key("dev"));
    }

    // -----------------------------------------------------------------------
    // Contract regression tests: env vars must NEVER override credentials.
    //
    // Background: a user reported "profile switching is broken because env
    // vars override profiles." That report is FALSE for the current code, and
    // these tests lock the contract in so it can never silently regress.
    // `resolve_profile()` only consults env-derived inputs for proxy/TLS
    // settings — never for instance, username, password, or profile name.
    // -----------------------------------------------------------------------

    #[test]
    fn env_vars_never_leak_into_credentials() {
        // Structural guarantee: `ProfileResolverInputs` has no
        // `env_username` / `env_password` / `env_instance` field. There is
        // deliberately no env-driven path for credential or instance
        // selection — the only env-driven inputs are proxy/TLS related
        // (`env_proxy`, `env_no_proxy`, `env_insecure`, `env_ca_cert`,
        // `env_proxy_ca_cert`). This test verifies that even when those env
        // inputs are set to garbage, the resolved instance/username/password
        // come exclusively from the profile file.
        let cfg = sample_config();
        let cr = sample_credentials();
        let r = resolve_profile(ProfileResolverInputs {
            cli_profile: Some("dev"),
            env_proxy: Some("http://garbage.invalid:9999"),
            env_no_proxy: Some("garbage"),
            env_insecure: Some("1"),
            env_ca_cert: Some("/garbage/ca.pem"),
            env_proxy_ca_cert: Some("/garbage/proxy-ca.pem"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert_eq!(r.name, "dev");
        assert_eq!(r.instance, "dev.example.com");
        assert_eq!(r.username, "dev-u");
        assert_eq!(r.password, "dev-p");
    }

    #[test]
    fn proxy_precedence_cli_beats_env_beats_profile() {
        let mut cfg = sample_config();
        cfg.profiles.get_mut("dev").unwrap().proxy = Some("http://profile.proxy:1".into());
        let cr = sample_credentials();

        // CLI > env > profile.
        let r = resolve_profile(ProfileResolverInputs {
            cli_proxy: Some("http://cli.proxy:3"),
            env_proxy: Some("http://env.proxy:2"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert_eq!(r.proxy.as_deref(), Some("http://cli.proxy:3"));

        // Drop CLI: env wins.
        let r = resolve_profile(ProfileResolverInputs {
            env_proxy: Some("http://env.proxy:2"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert_eq!(r.proxy.as_deref(), Some("http://env.proxy:2"));

        // Drop env: profile wins.
        let r = resolve_profile(base_inputs(&cfg, &cr)).unwrap();
        assert_eq!(r.proxy.as_deref(), Some("http://profile.proxy:1"));
    }

    #[test]
    fn no_proxy_flag_clears_all_proxies() {
        let mut cfg = sample_config();
        cfg.profiles.get_mut("dev").unwrap().proxy = Some("http://profile.proxy:1".into());
        let cr = sample_credentials();
        let r = resolve_profile(ProfileResolverInputs {
            cli_proxy: Some("http://cli.proxy:3"),
            env_proxy: Some("http://env.proxy:2"),
            cli_no_proxy: true,
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert_eq!(r.proxy, None);
    }

    #[test]
    fn env_no_proxy_string_propagates() {
        let cfg = sample_config();
        let cr = sample_credentials();
        let r = resolve_profile(ProfileResolverInputs {
            env_no_proxy: Some("localhost,127.0.0.1"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert_eq!(r.no_proxy.as_deref(), Some("localhost,127.0.0.1"));
    }

    #[test]
    fn env_insecure_recognizes_1_and_true() {
        let cfg = sample_config();
        let cr = sample_credentials();

        // "1" flips the flag.
        let r = resolve_profile(ProfileResolverInputs {
            env_insecure: Some("1"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert!(r.insecure);

        // "true" (lowercase) flips the flag.
        let r = resolve_profile(ProfileResolverInputs {
            env_insecure: Some("true"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert!(r.insecure);

        // "TRUE" (uppercase) flips the flag.
        let r = resolve_profile(ProfileResolverInputs {
            env_insecure: Some("TRUE"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert!(r.insecure);

        // "0" does NOT flip the flag.
        let r = resolve_profile(ProfileResolverInputs {
            env_insecure: Some("0"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert!(!r.insecure);

        // "false" does NOT flip the flag.
        let r = resolve_profile(ProfileResolverInputs {
            env_insecure: Some("false"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert!(!r.insecure);

        // Arbitrary garbage does NOT flip the flag.
        let r = resolve_profile(ProfileResolverInputs {
            env_insecure: Some("yes-please"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert!(!r.insecure);
    }

    #[test]
    fn cli_insecure_or_env_or_profile_ored() {
        let mut cfg = sample_config();
        let cr = sample_credentials();

        // CLI alone is enough.
        let r = resolve_profile(ProfileResolverInputs {
            cli_insecure: true,
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert!(r.insecure);

        // Env alone is enough.
        let r = resolve_profile(ProfileResolverInputs {
            env_insecure: Some("1"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert!(r.insecure);

        // Profile alone is enough.
        cfg.profiles.get_mut("dev").unwrap().insecure = true;
        let r = resolve_profile(base_inputs(&cfg, &cr)).unwrap();
        assert!(r.insecure);

        // All three false/unset → resolved is false.
        cfg.profiles.get_mut("dev").unwrap().insecure = false;
        let r = resolve_profile(base_inputs(&cfg, &cr)).unwrap();
        assert!(!r.insecure);
    }

    #[test]
    fn ca_cert_precedence_cli_beats_env_beats_profile() {
        let mut cfg = sample_config();
        cfg.profiles.get_mut("dev").unwrap().ca_cert = Some("/profile/ca.pem".into());
        let cr = sample_credentials();

        // CLI > env > profile.
        let r = resolve_profile(ProfileResolverInputs {
            cli_ca_cert: Some("/cli/ca.pem"),
            env_ca_cert: Some("/env/ca.pem"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert_eq!(r.ca_cert.as_deref(), Some("/cli/ca.pem"));

        // Drop CLI: env wins.
        let r = resolve_profile(ProfileResolverInputs {
            env_ca_cert: Some("/env/ca.pem"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert_eq!(r.ca_cert.as_deref(), Some("/env/ca.pem"));

        // Drop env: profile wins.
        let r = resolve_profile(base_inputs(&cfg, &cr)).unwrap();
        assert_eq!(r.ca_cert.as_deref(), Some("/profile/ca.pem"));
    }

    #[test]
    fn proxy_ca_cert_precedence_cli_beats_env_beats_profile() {
        let mut cfg = sample_config();
        cfg.profiles.get_mut("dev").unwrap().proxy_ca_cert = Some("/profile/proxy-ca.pem".into());
        let cr = sample_credentials();

        // CLI > env > profile.
        let r = resolve_profile(ProfileResolverInputs {
            cli_proxy_ca_cert: Some("/cli/proxy-ca.pem"),
            env_proxy_ca_cert: Some("/env/proxy-ca.pem"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert_eq!(r.proxy_ca_cert.as_deref(), Some("/cli/proxy-ca.pem"));

        // Drop CLI: env wins.
        let r = resolve_profile(ProfileResolverInputs {
            env_proxy_ca_cert: Some("/env/proxy-ca.pem"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert_eq!(r.proxy_ca_cert.as_deref(), Some("/env/proxy-ca.pem"));

        // Drop env: profile wins.
        let r = resolve_profile(base_inputs(&cfg, &cr)).unwrap();
        assert_eq!(r.proxy_ca_cert.as_deref(), Some("/profile/proxy-ca.pem"));
    }

    #[test]
    fn proxy_credentials_only_come_from_profile_file() {
        let cfg = sample_config();
        let mut cr = sample_credentials();
        let dev = cr.profiles.get_mut("dev").unwrap();
        dev.proxy_username = Some("proxy-user".into());
        dev.proxy_password = Some("proxy-pass".into());

        let r = resolve_profile(base_inputs(&cfg, &cr)).unwrap();
        assert_eq!(r.proxy_username.as_deref(), Some("proxy-user"));
        assert_eq!(r.proxy_password.as_deref(), Some("proxy-pass"));
    }

    #[test]
    fn unknown_profile_name_falls_through_to_missing_field_error() {
        let cfg = sample_config();
        let cr = sample_credentials();
        let err = resolve_profile(ProfileResolverInputs {
            cli_profile: Some("nonexistent"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap_err();
        match err {
            Error::Config(msg) => assert!(
                msg.contains("nonexistent"),
                "error message should name the profile, got: {msg}"
            ),
            other => panic!("expected Error::Config, got: {other:?}"),
        }
    }

    #[test]
    fn default_profile_in_config_used_when_no_cli_profile() {
        let mut cfg = sample_config();
        cfg.default_profile = Some("prod".into());
        let cr = sample_credentials();
        let r = resolve_profile(base_inputs(&cfg, &cr)).unwrap();
        assert_eq!(r.name, "prod");
        assert_eq!(r.instance, "prod.example.com");
    }

    #[test]
    fn oauth_profile_resolves_without_requiring_username_password() {
        let mut cfg = Config {
            default_profile: Some("sso".into()),
            ..Default::default()
        };
        cfg.profiles.insert(
            "sso".into(),
            ProfileConfig {
                instance: "sso.example.com".into(),
                auth: AuthMethod::Oauth,
                oauth: Some(OAuthConfig {
                    client_id: "abc".into(),
                    redirect_uri: None,
                    auth_path: None,
                    token_path: None,
                    grant: OAuthGrant::AuthorizationCode,
                    pkce: true,
                }),
                ..Default::default()
            },
        );
        let mut cr = Credentials::default();
        cr.profiles.insert(
            "sso".into(),
            ProfileCredentials {
                client_secret: Some("shh".into()),
                oauth_tokens: Some(TokenSet {
                    access_token: "AT".into(),
                    refresh_token: Some("RT".into()),
                    expires_at: Some(now_unix() + 3600),
                    token_type: Some("Bearer".into()),
                }),
                ..Default::default()
            },
        );
        let r = resolve_profile(base_inputs(&cfg, &cr)).unwrap();
        assert_eq!(r.name, "sso");
        assert_eq!(r.auth_method, AuthMethod::Oauth);
        let o = r.oauth.expect("resolved oauth state");
        assert_eq!(o.client_id, "abc");
        // Defaults applied for the unset endpoint/redirect fields.
        assert_eq!(o.redirect_uri, default_redirect_uri());
        assert_eq!(o.auth_path, "/oauth_auth.do");
        assert_eq!(o.token_path, "/oauth_token.do");
        assert_eq!(o.client_secret.as_deref(), Some("shh"));
        assert_eq!(o.tokens.unwrap().access_token, "AT");
    }

    #[test]
    fn oauth_profile_without_config_section_errors() {
        let mut cfg = Config::default();
        cfg.profiles.insert(
            "sso".into(),
            ProfileConfig {
                instance: "sso.example.com".into(),
                auth: AuthMethod::Oauth,
                oauth: None,
                ..Default::default()
            },
        );
        let cr = Credentials::default();
        let err = resolve_profile(ProfileResolverInputs {
            cli_profile: Some("sso"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn cli_profile_overrides_default_profile_in_config() {
        let mut cfg = sample_config();
        cfg.default_profile = Some("prod".into());
        let cr = sample_credentials();
        let r = resolve_profile(ProfileResolverInputs {
            cli_profile: Some("dev"),
            ..base_inputs(&cfg, &cr)
        })
        .unwrap();
        assert_eq!(r.name, "dev");
        assert_eq!(r.instance, "dev.example.com");
    }
}
