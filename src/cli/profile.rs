use crate::cli::auth::{complete_oauth_login, whoami};
use crate::cli::kernel::{build_client, build_profile, confirm_destructive, write_response};
use crate::cli::GlobalFlags;
use crate::config::{
    self, config_path, credentials_path, default_redirect_uri, load_config_from,
    load_credentials_from, resolve_profile_name, save_config_to, save_credentials_to, AuthMethod,
    OAuthConfig, OAuthGrant, ProfileConfig, ProfileCredentials,
};
use crate::error::{Error, Result};
use clap::Subcommand;
use is_terminal::IsTerminal;
use serde_json::json;
use std::io::{self, Read, Write};

#[derive(Subcommand, Debug)]
pub enum ProfileSub {
    /// Add a profile. Leaves the default profile alone (see `sn profile use`).
    Add(ProfileAddArgs),
    /// List every configured profile.
    List,
    /// Show one profile's settings (secrets redacted).
    Show {
        /// Profile to show; defaults to the selected profile.
        name: Option<String>,
    },
    /// Delete a profile and its stored credentials.
    Remove {
        /// Profile to remove.
        name: String,
        /// Skip confirmation prompt (required for non-interactive use).
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Make a profile the default for subsequent commands.
    Use {
        /// Profile to make default.
        name: String,
    },
    /// Run the OAuth flow for the selected (already-configured) profile and cache tokens.
    Login,
    /// Discard the profile's cached OAuth tokens.
    Logout,
    /// Show the resolved auth method and OAuth token status for a profile.
    Status,
    /// Force an OAuth token refresh now.
    Refresh,
}

#[derive(clap::Args, Debug)]
pub struct ProfileAddArgs {
    /// Profile name. Falls back to `--profile`; prompted when interactive.
    #[arg(value_name = "NAME")]
    pub name: Option<String>,
    /// Instance: short name (`dev380385`) or full URL.
    #[arg(long)]
    pub instance: Option<String>,
    /// Authentication method: `basic` (username/password) or `oauth` (SSO / OAuth 2.0).
    #[arg(long, value_enum)]
    pub auth: Option<AuthMethod>,
    /// Username (basic auth only).
    #[arg(long)]
    pub username: Option<String>,
    /// Password (basic auth only). Visible in `ps` output and shell history —
    /// prefer `--password-stdin`.
    #[arg(long, conflicts_with = "password_stdin")]
    pub password: Option<String>,
    /// Read the password from stdin (basic auth only).
    #[arg(long, conflicts_with = "client_secret_stdin")]
    pub password_stdin: bool,
    /// OAuth client_id (oauth only).
    #[arg(long)]
    pub client_id: Option<String>,
    /// OAuth client secret (oauth confidential clients). Visible in `ps` output
    /// and shell history — prefer `--client-secret-stdin`.
    #[arg(long, conflicts_with = "client_secret_stdin")]
    pub client_secret: Option<String>,
    /// Read the OAuth client secret from stdin.
    #[arg(long)]
    pub client_secret_stdin: bool,
    /// OAuth loopback redirect URI (oauth only). Defaults to http://localhost:8400/callback.
    #[arg(long, value_name = "URL")]
    pub redirect_uri: Option<String>,
    /// OAuth grant: authorization_code (SSO, default) or client_credentials.
    #[arg(long, value_enum)]
    pub grant: Option<OAuthGrant>,
    /// Disable PKCE for the authorization-code flow.
    #[arg(long)]
    pub no_pkce: bool,
    /// Overwrite the profile if it already exists.
    #[arg(long)]
    pub force: bool,
    /// Save without checking the credentials against the instance.
    #[arg(long)]
    pub no_verify: bool,
    /// Also make this the default profile.
    #[arg(long)]
    pub set_default: bool,
    /// Never prompt; fail naming the missing flag instead.
    #[arg(long)]
    pub non_interactive: bool,
}

pub fn run(global: &GlobalFlags, sub: ProfileSub) -> Result<()> {
    match sub {
        ProfileSub::Add(args) => add(global, args),
        ProfileSub::List => list(global),
        ProfileSub::Show { name } => show(global, name),
        ProfileSub::Remove { name, yes } => remove(global, name, yes),
        ProfileSub::Use { name } => set_default(global, name),
        ProfileSub::Login => crate::cli::auth::login(global),
        ProfileSub::Logout => crate::cli::auth::logout(global),
        ProfileSub::Status => crate::cli::auth::status(global),
        ProfileSub::Refresh => crate::cli::auth::refresh(global),
    }
}

fn auth_str(method: AuthMethod) -> &'static str {
    match method {
        AuthMethod::Basic => "basic",
        AuthMethod::Oauth => "oauth",
    }
}

// ---------------------------------------------------------------------------
// Shared profile-writing core, used by `sn profile add` and `sn init`.
//
// The two commands differ only in policy — `add` refuses to clobber and never
// touches `default_profile`; `init` upserts and always claims the default — so
// everything below is policy-free and takes the decision as an argument.
// ---------------------------------------------------------------------------

/// Which command's flag vocabulary the core should speak when a field is
/// missing and there is nobody to prompt.
///
/// `sn init` and `sn profile add` share this code but not their argument lists:
/// `init` has no `--password-stdin`, `--client-secret-stdin` or
/// `--non-interactive`. An error naming a flag the invoked command does not
/// accept sends the caller to a dead end, so every message is phrased for the
/// command that was actually run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Caller {
    Init,
    ProfileAdd,
}

impl Caller {
    /// The other way to reach a non-interactive run, when the command has one.
    fn non_interactive_hint(self) -> &'static str {
        match self {
            Caller::Init => "",
            Caller::ProfileAdd => " (or with --non-interactive)",
        }
    }

    fn password_flag(self) -> &'static str {
        match self {
            Caller::Init => "--password",
            Caller::ProfileAdd => "--password (or --password-stdin)",
        }
    }

    fn client_secret_flag(self) -> &'static str {
        match self {
            Caller::Init => "--client-secret",
            Caller::ProfileAdd => "--client-secret (or --client-secret-stdin)",
        }
    }
}

/// One profile's worth of settings, with every prompt and flag already resolved.
pub(crate) struct ProfileInput {
    pub name: String,
    pub instance: String,
    pub auth: AuthMethod,
    pub username: String,
    pub password: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: Option<String>,
    pub grant: OAuthGrant,
    pub pkce: bool,
}

/// What one [`save_profile`] displaced, so [`restore`] can put back exactly
/// that much when verification fails.
///
/// Deliberately **not** a copy of both whole files. Verification is a network
/// round trip, so the rollback lands some seconds after the write; restoring
/// two entire files would then stomp any profile a sibling `sn` invocation
/// added in between — trading one dead profile for someone else's live one.
/// Only the entry this run touched is reverted, and `default_profile` only if
/// this run is still the one holding it.
pub(crate) struct Snapshot {
    /// The profile this run wrote.
    name: String,
    /// Its `config.toml` entry beforehand; `None` if it did not exist.
    config: Option<ProfileConfig>,
    /// Its `credentials.toml` entry beforehand; `None` if it did not exist.
    credentials: Option<ProfileCredentials>,
    /// `default_profile` beforehand — only consulted when `claimed_default`.
    prior_default: Option<String>,
    /// Whether this run pointed `default_profile` at `name`.
    claimed_default: bool,
}

/// True when there is a human on the other end of stdin to prompt.
fn is_interactive(args: &ProfileAddArgs) -> bool {
    !args.non_interactive && io::stdin().is_terminal()
}

/// Resolve just the profile name, so a caller can decide whether it may write to
/// that name *before* asking for an instance and a password it would then throw
/// away. `name_default` is the name to assume when none is given and there is
/// nobody to ask (`sn init` uses "default"; `sn profile add` has none, so the
/// name is required).
pub(crate) fn resolve_name(
    args: &ProfileAddArgs,
    name_default: Option<String>,
    caller: Caller,
) -> Result<String> {
    // Show the fallback in the prompt when there is one, so `sn init` still reads
    // `Profile name [default]:` and the reader knows what Enter will do.
    let msg = match &name_default {
        Some(d) => format!("Profile name [{d}]: "),
        None => "Profile name: ".to_string(),
    };
    let name = match &args.name {
        Some(v) => v.clone(),
        None => ask(is_interactive(args), &msg, name_default, "NAME", caller)?,
    };
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(Error::Usage("profile name is required".into()));
    }
    Ok(name)
}

/// Turn args + prompts into a [`ProfileInput`] for the already-resolved `name`.
///
/// Prompting only ever happens on a terminal: with `--non-interactive`, or when
/// stdin is a pipe, a field with no default names the flag that would supply it
/// rather than blocking on a read that will never be answered.
pub(crate) fn resolve_input(
    args: &ProfileAddArgs,
    name: String,
    caller: Caller,
) -> Result<ProfileInput> {
    let interactive = is_interactive(args);

    let instance = match &args.instance {
        Some(v) => v.clone(),
        None => ask(
            interactive,
            "Instance (e.g. 'dev380385' or 'https://acme.service-now.com'): ",
            None,
            "--instance",
            caller,
        )?,
    };
    let instance = normalize_instance(&instance);
    if instance.is_empty() {
        return Err(Error::Usage("instance is required".into()));
    }

    // Basic is the default everywhere else (`AuthMethod::default()`), so a
    // scripted call needn't spell it out.
    let auth = match args.auth {
        Some(m) => m,
        None => match ask(
            interactive,
            "Auth method (basic/oauth) [basic]: ",
            Some("basic".into()),
            "--auth",
            caller,
        )?
        .to_ascii_lowercase()
        .as_str()
        {
            "oauth" => AuthMethod::Oauth,
            "basic" => AuthMethod::Basic,
            other => {
                return Err(Error::Usage(format!(
                    "unknown auth method '{other}' (expected basic or oauth)"
                )))
            }
        },
    };

    let mut input = ProfileInput {
        name,
        instance,
        auth,
        username: String::new(),
        password: String::new(),
        client_id: String::new(),
        client_secret: None,
        redirect_uri: None,
        grant: OAuthGrant::default(),
        pkce: !args.no_pkce,
    };

    match auth {
        AuthMethod::Basic => {
            input.username = match &args.username {
                Some(v) => v.clone(),
                None => ask(interactive, "Username: ", None, "--username", caller)?,
            };
            input.password = match (&args.password, args.password_stdin) {
                (Some(v), _) => v.clone(),
                (None, true) => read_secret_stdin()?,
                (None, false) if interactive => rpassword::prompt_password("Password: ")
                    .map_err(|e| Error::Usage(format!("read password: {e}")))?,
                (None, false) => return Err(missing(caller.password_flag(), caller)),
            };
            if input.username.trim().is_empty() || input.password.is_empty() {
                return Err(Error::Usage(
                    "username and password are required for basic auth".into(),
                ));
            }
        }
        AuthMethod::Oauth => {
            input.client_id = match &args.client_id {
                Some(v) => v.clone(),
                None => ask(
                    interactive,
                    "OAuth client_id: ",
                    None,
                    "--client-id",
                    caller,
                )?,
            };
            if input.client_id.trim().is_empty() {
                return Err(Error::Usage("client_id is required for oauth".into()));
            }
            input.grant = args.grant.unwrap_or_default();

            // authorization_code registers a PUBLIC client (PKCE, no secret), so
            // never prompt for a secret on that path — pass --client-secret
            // explicitly only for a confidential client. client_credentials is
            // inherently confidential and must have one.
            input.client_secret = match (
                &args.client_secret,
                args.client_secret_stdin,
                input.grant,
                interactive,
            ) {
                (Some(s), ..) => Some(s.clone()),
                (None, true, ..) => Some(read_secret_stdin()?),
                (None, false, OAuthGrant::ClientCredentials, true) => {
                    let s = rpassword::prompt_password("OAuth client_secret: ")
                        .map_err(|e| Error::Usage(format!("read secret: {e}")))?;
                    if s.is_empty() {
                        return Err(Error::Usage(
                            "client_credentials grant requires a client secret".into(),
                        ));
                    }
                    Some(s)
                }
                (None, false, OAuthGrant::ClientCredentials, false) => {
                    return Err(missing(caller.client_secret_flag(), caller))
                }
                (None, false, OAuthGrant::AuthorizationCode, _) => None,
            };

            // The loopback redirect only exists in the browser flow; don't ask
            // for one under client_credentials. Left unset, `resolve_profile`
            // applies `default_redirect_uri()`.
            input.redirect_uri = match (&args.redirect_uri, input.grant) {
                (Some(u), _) => Some(u.clone()),
                (None, OAuthGrant::ClientCredentials) => None,
                (None, OAuthGrant::AuthorizationCode) if interactive => {
                    let d = default_redirect_uri();
                    Some(ask(
                        true,
                        &format!("Redirect URI [{d}]: "),
                        Some(d),
                        "--redirect-uri",
                        caller,
                    )?)
                }
                (None, OAuthGrant::AuthorizationCode) => None,
            };
        }
    }

    Ok(input)
}

/// Merge `input` into `config.toml` + `credentials.toml` and persist both.
///
/// Non-destructive: the write starts from whatever is stored and overwrites only
/// what this run configures, so fields neither command can set — proxy
/// credentials, OAuth endpoint overrides — survive. Returns the prior state for
/// [`restore`].
///
/// The entire read → modify → write runs inside `with_config_lock`. Locking
/// only the two `save_*` calls would not help at all: two `sn profile add`
/// invocations would each read the same old config, each add their own profile
/// to it, and each write their copy back — the second erasing the first. That
/// is not theoretical, it is what twelve parallel adds did before this lock
/// (one survivor, eleven silent losses, all twelve exiting 0).
///
/// `credentials.toml` is written **first**, `config.toml` second, because
/// readers hold no lock and profile resolution is keyed off `config.toml`: a
/// reader between the two renames must not find a config entry whose
/// credentials have not landed, which would resolve to an empty
/// username/password and a baffling 401. The other order is harmless — the
/// profile simply reads as not yet existing.
fn save_profile(
    global: &GlobalFlags,
    input: &ProfileInput,
    set_default: bool,
    refuse_existing: bool,
) -> Result<Snapshot> {
    config::with_config_lock(|| save_profile_locked(global, input, set_default, refuse_existing))
}

fn save_profile_locked(
    global: &GlobalFlags,
    input: &ProfileInput,
    set_default: bool,
    refuse_existing: bool,
) -> Result<Snapshot> {
    let cfg_path = config_path()?;
    let cred_path = credentials_path()?;
    let mut config = load_config_from(&cfg_path)?;
    let mut creds = load_credentials_from(&cred_path)?;

    // `sn profile add`'s refusal to clobber is re-checked here, under the lock,
    // and not only in `add` before prompting: that earlier check reads the file
    // and then spends a prompt's worth of time before writing, so on its own it
    // would let two concurrent adds of one name both decide the name was free.
    if refuse_existing && config.profiles.contains_key(&input.name) {
        return Err(Error::Usage(format!(
            "profile '{}' already exists; pass --force to overwrite it",
            input.name
        )));
    }

    let snapshot = Snapshot {
        name: input.name.clone(),
        config: config.profiles.get(&input.name).cloned(),
        credentials: creds.profiles.get(&input.name).cloned(),
        prior_default: config.default_profile.clone(),
        claimed_default: set_default,
    };

    let mut pc: ProfileConfig = config
        .profiles
        .get(&input.name)
        .cloned()
        .unwrap_or_default();
    let mut cred: ProfileCredentials = creds.profiles.get(&input.name).cloned().unwrap_or_default();

    pc.instance = input.instance.clone();

    // Proxy/TLS settings from the global flags are persisted with the profile so
    // later invocations pick them up automatically. Flags that were not passed
    // leave the stored values alone.
    if global.no_proxy {
        pc.proxy = None;
    } else if let Some(proxy) = &global.proxy {
        pc.proxy = Some(proxy.clone());
    }
    if global.insecure {
        pc.insecure = true;
    }
    if let Some(ca_cert) = &global.ca_cert {
        pc.ca_cert = Some(ca_cert.clone());
    }
    if let Some(proxy_ca_cert) = &global.proxy_ca_cert {
        pc.proxy_ca_cert = Some(proxy_ca_cert.clone());
    }

    match input.auth {
        AuthMethod::Basic => {
            // Switching an OAuth profile to basic: drop the OAuth config and its
            // secrets so the abandoned method can't leak or confuse.
            pc.auth = AuthMethod::Basic;
            pc.oauth = None;
            cred.client_secret = None;
            cred.oauth_tokens = None;
            cred.username = input.username.clone();
            cred.password = input.password.clone();
        }
        AuthMethod::Oauth => {
            let existing = pc.oauth.take();
            pc.auth = AuthMethod::Oauth;
            pc.oauth = Some(OAuthConfig {
                client_id: input.client_id.clone(),
                redirect_uri: input.redirect_uri.clone(),
                auth_path: existing.as_ref().and_then(|o| o.auth_path.clone()),
                token_path: existing.as_ref().and_then(|o| o.token_path.clone()),
                grant: input.grant,
                pkce: input.pkce,
            });
            // Switching a basic profile to oauth clears the now-unused password.
            cred.password = String::new();
            cred.client_secret = input.client_secret.clone();
        }
    }

    if set_default {
        config.default_profile = Some(input.name.clone());
    }
    config.profiles.insert(input.name.clone(), pc);
    creds.profiles.insert(input.name.clone(), cred);
    save_credentials_to(&cred_path, &creds)?;
    save_config_to(&cfg_path, &config)?;
    Ok(snapshot)
}

/// Undo one [`save_profile`], touching only what it wrote.
///
/// Re-reads both files under the lock rather than replaying a whole-file
/// snapshot, so a profile some other invocation added while this one was
/// verifying survives the rollback. `default_profile` goes back only if it is
/// still pointing at the profile this run claimed it for — otherwise somebody
/// ran `sn profile use` in the meantime and their choice wins.
///
/// Write order is the mirror of `save_profile`'s: `config.toml` first, so the
/// window an unlocked reader can catch shows the profile already gone from
/// config rather than present with its credentials removed.
fn restore(snap: &Snapshot) -> Result<()> {
    config::with_config_lock(|| {
        let cfg_path = config_path()?;
        let cred_path = credentials_path()?;
        let mut config = load_config_from(&cfg_path)?;
        let mut creds = load_credentials_from(&cred_path)?;

        match &snap.config {
            Some(pc) => {
                config.profiles.insert(snap.name.clone(), pc.clone());
            }
            None => {
                config.profiles.remove(&snap.name);
            }
        }
        match &snap.credentials {
            Some(cred) => {
                creds.profiles.insert(snap.name.clone(), cred.clone());
            }
            None => {
                creds.profiles.remove(&snap.name);
            }
        }
        if snap.claimed_default && config.default_profile.as_deref() == Some(snap.name.as_str()) {
            config.default_profile = snap.prior_default.clone();
        }

        save_config_to(&cfg_path, &config)?;
        save_credentials_to(&cred_path, &creds)
    })
}

/// Prove a freshly-saved profile actually authenticates, returning the
/// `user_name` the instance reported (when it surfaced one).
///
/// Basic does an authenticated read. OAuth delegates to `complete_oauth_login`,
/// which runs the profile's grant — a browser for `authorization_code`, a
/// headless mint for `client_credentials` — caches the tokens, and verifies them.
fn verify_profile(
    global: &GlobalFlags,
    name: &str,
    auth: AuthMethod,
    grant: OAuthGrant,
) -> Result<Option<String>> {
    match auth {
        AuthMethod::Basic => {
            // Scope resolution to this profile, independent of which one happens
            // to be the global default.
            let mut scoped = global.clone();
            scoped.profile = Some(name.to_string());
            let profile = build_profile(&scoped)?;
            let client = build_client(&profile, scoped.timeout)?;
            whoami(&client)
        }
        AuthMethod::Oauth => complete_oauth_login(global, name, grant).map(|(_, user)| user),
    }
}

/// The three policies that separate `sn init` from `sn profile add`, in the one
/// place both funnel through.
pub(crate) struct SavePolicy {
    /// Point `default_profile` at this profile.
    pub set_default: bool,
    /// Prove the credentials authenticate, and roll back if they don't.
    pub verify: bool,
    /// Fail rather than overwrite an existing profile.
    pub refuse_existing: bool,
}

/// Persist `input`, then prove it works — rolling the write back if it doesn't,
/// so a profile that cannot authenticate never survives on disk. An unverified
/// profile is worse than no profile: it fails later, somewhere confusing.
///
/// Verification runs *outside* the config lock on purpose. It is a network
/// round trip (and for `authorization_code`, a browser and a human), so holding
/// the lock across it would park every other `sn` invocation on this machine
/// behind an SSO prompt until the 10s lock timeout fired. What makes that safe
/// is that the rollback is per-profile rather than a whole-file snapshot; see
/// [`restore`].
pub(crate) fn save_and_verify(
    global: &GlobalFlags,
    input: &ProfileInput,
    policy: SavePolicy,
) -> Result<Option<String>> {
    let snapshot = save_profile(global, input, policy.set_default, policy.refuse_existing)?;
    if !policy.verify {
        return Ok(None);
    }
    match verify_profile(global, &input.name, input.auth, input.grant) {
        Ok(user) => Ok(user),
        Err(e) => {
            // Best-effort rollback; the verification failure is the error worth
            // reporting either way.
            let _ = restore(&snapshot);
            Err(e)
        }
    }
}

/// Prompt for one field on a terminal.
///
/// With a `default`, an empty answer — or a non-interactive run, where there is
/// nobody to ask — resolves to it. Without one the field is required, and a
/// non-interactive run names the flag that supplies it instead of hanging on a
/// stdin nobody will type into.
fn ask(
    interactive: bool,
    msg: &str,
    default: Option<String>,
    flag: &str,
    caller: Caller,
) -> Result<String> {
    if !interactive {
        return default.ok_or_else(|| missing(flag, caller));
    }
    print!("{msg}");
    io::stdout().flush().ok();
    let mut s = String::new();
    io::stdin()
        .read_line(&mut s)
        .map_err(|e| Error::Usage(format!("read stdin: {e}")))?;
    let trimmed = s.trim().to_string();
    Ok(if trimmed.is_empty() {
        default.unwrap_or_default()
    } else {
        trimmed
    })
}

fn missing(flag: &str, caller: Caller) -> Error {
    Error::Usage(format!(
        "{flag} is required when stdin is not a terminal{}",
        caller.non_interactive_hint()
    ))
}

/// Read a secret from stdin, the way `docker login --password-stdin` does: it
/// keeps the secret out of `ps` output and shell history, which an argv flag
/// cannot. Only the trailing newline is stripped — a password may legitimately
/// begin or end with a space.
fn read_secret_stdin() -> Result<String> {
    let mut s = String::new();
    io::stdin()
        .read_to_string(&mut s)
        .map_err(|e| Error::Usage(format!("read stdin: {e}")))?;
    Ok(s.trim_end_matches(['\n', '\r']).to_string())
}

/// Turn whatever the user typed into something `client.rs::normalize_base_url`
/// can use. A short instance name like `dev380385` becomes `dev380385.service-now.com`;
/// anything that already looks like a URL or FQDN is passed through untouched
/// (modulo a trailing slash).
fn normalize_instance(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") || trimmed.contains('.') {
        trimmed.to_string()
    } else {
        format!("{trimmed}.service-now.com")
    }
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

/// `sn profile add` — register one profile and prove it works.
///
/// Deliberately narrower than `sn init`: it refuses to overwrite an identity
/// somebody may be relying on (`--force` opts in), and it never changes which
/// profile commands resolve to by default (`--set-default` or `sn profile use`
/// does that).
fn add(global: &GlobalFlags, args: ProfileAddArgs) -> Result<()> {
    let mut args = args;
    // A bare `--profile x` is a reasonable way to name the thing being added,
    // but the positional wins.
    if args.name.is_none() {
        args.name = global.profile.clone();
    }

    // Settle the name first: refusing to clobber is only useful if it happens
    // before we prompt for an instance and a password we would then discard.
    let name = resolve_name(&args, None, Caller::ProfileAdd)?;
    let existing = load_config_from(&config_path()?)?;
    if existing.profiles.contains_key(&name) && !args.force {
        return Err(Error::Usage(format!(
            "profile '{name}' already exists; pass --force to overwrite it"
        )));
    }

    let input = resolve_input(&args, name, Caller::ProfileAdd)?;

    // The browser flow is the only way to test authorization_code credentials,
    // and there is no browser to open here. Refuse rather than save a profile
    // whose credentials were never checked.
    let needs_browser = matches!(input.auth, AuthMethod::Oauth)
        && matches!(input.grant, OAuthGrant::AuthorizationCode);
    if needs_browser && !is_interactive(&args) && !args.no_verify {
        return Err(Error::Usage(format!(
            "the authorization_code grant cannot be verified without a browser; \
             re-run with --no-verify, then run `sn profile login --profile {}`",
            input.name
        )));
    }

    let user = save_and_verify(
        global,
        &input,
        SavePolicy {
            set_default: args.set_default,
            verify: !args.no_verify,
            refuse_existing: !args.force,
        },
    )?;

    let mut out = json!({
        "ok": true,
        "profile": input.name,
        "instance": input.instance,
        "auth": auth_str(input.auth),
        "verified": !args.no_verify,
    });
    if matches!(input.auth, AuthMethod::Oauth) {
        out["grant"] = json!(input.grant.as_str());
        out["loggedIn"] = json!(!args.no_verify);
        if args.no_verify {
            out["next"] = json!(format!("sn profile login --profile {}", input.name));
        }
    }
    if let Some(u) = user {
        out["user"] = json!(u);
    }

    // `add` doesn't touch the default profile, so say when nothing is selected —
    // otherwise the very next command fails with "no profile selected". A pending
    // login outranks it: an OAuth profile with no tokens can't be used even with
    // `--profile`, whereas an unselected one can.
    let config = load_config_from(&config_path()?)?;
    out["default"] = json!(config.default_profile.as_deref() == Some(input.name.as_str()));
    if config.default_profile.is_none() && out.get("next").is_none() {
        out["next"] = json!(format!("sn profile use {}", input.name));
    }

    write_response(global, &out)
}

fn list(global: &GlobalFlags) -> Result<()> {
    let cfg = load_config_from(&config_path()?)?;
    let profiles: Vec<serde_json::Value> = cfg
        .profiles
        .iter()
        .map(|(name, p)| {
            json!({
                "name": name,
                "instance": p.instance,
                "auth": auth_str(p.auth),
                "default": cfg.default_profile.as_deref() == Some(name.as_str()),
            })
        })
        .collect();
    write_response(global, &json!(profiles))
}

fn show(global: &GlobalFlags, name: Option<String>) -> Result<()> {
    let cfg = load_config_from(&config_path()?)?;
    let name = resolve_profile_name(name.as_deref(), &cfg)?;
    let p: &ProfileConfig = cfg
        .profiles
        .get(&name)
        .ok_or_else(|| Error::Usage(format!("profile '{name}' not found")))?;
    let creds = load_credentials_from(&credentials_path()?)?;
    let cred = creds.profiles.get(&name);

    // Names, instances, and OAuth client config are fine to print; passwords,
    // client secrets, and token values are NEVER emitted here.
    let mut out = json!({
        "name": name,
        "instance": p.instance,
        "auth": auth_str(p.auth),
    });
    match p.auth {
        AuthMethod::Basic => {
            if let Some(c) = cred {
                out["username"] = json!(c.username);
            }
        }
        AuthMethod::Oauth => {
            if let Some(o) = &p.oauth {
                out["client_id"] = json!(o.client_id);
                out["grant"] = json!(o.grant.as_str());
                out["redirect_uri"] =
                    json!(o.redirect_uri.clone().unwrap_or_else(default_redirect_uri));
                out["pkce"] = json!(o.pkce);
            }
            let tokens = cred.and_then(|c| c.oauth_tokens.as_ref());
            out["loggedIn"] = json!(tokens.is_some());
            if let Some(t) = tokens {
                out["hasRefreshToken"] = json!(t.refresh_token.is_some());
                if let Some(exp) = t.expires_at {
                    out["expiresAt"] = json!(exp);
                }
            }
        }
    }
    write_response(global, &out)
}

/// `sn profile remove` — drop a profile from both files under one lock.
///
/// Deliberately idempotent — removing a profile that isn't there is not an
/// error — so the result says what actually happened rather than leaving the
/// caller to re-read the config to find out.
///
/// `config.toml` goes first, the mirror of `save_profile`'s order: an unlocked
/// reader that runs between the renames sees the profile already absent from
/// config, which resolves as "not found", rather than present with its
/// credentials already gone. See `config::LOCK_FILE` for why that is narrower
/// than the guarantee `save_profile` gets.
fn remove(global: &GlobalFlags, name: String, yes: bool) -> Result<()> {
    // The one destructive command with no remote copy to re-fetch from: the
    // password or OAuth tokens in credentials.toml exist nowhere else.
    confirm_destructive(
        yes,
        "remove",
        &format!("profile {name} and its stored credentials"),
    )?;
    // The lock spans the read as well as the writes: without it this would
    // write back a pair of files it read before a concurrent `sn profile add`
    // landed, deleting that profile. Emission stays outside — stdout is not
    // shared state and holding the lock across it would serialize writers
    // behind a slow pipe.
    let (removed, was_default) = config::with_config_lock(|| {
        let cfg_path = config_path()?;
        let cred_path = credentials_path()?;
        let mut cfg = load_config_from(&cfg_path)?;
        let mut creds = load_credentials_from(&cred_path)?;
        let had_config = cfg.profiles.remove(&name).is_some();
        let had_creds = creds.profiles.remove(&name).is_some();
        let was_default = cfg.default_profile.as_deref() == Some(&name);
        if was_default {
            cfg.default_profile = None;
        }
        save_config_to(&cfg_path, &cfg)?;
        save_credentials_to(&cred_path, &creds)?;
        Ok((had_config || had_creds, was_default))
    })?;

    let mut out = json!({
        "ok": true,
        "profile": name,
        "removed": removed,
        "wasDefault": was_default,
    });
    // Dropping the default leaves every later command with nothing to resolve,
    // which is worth saying here rather than at the next command's failure.
    if was_default {
        out["next"] = json!("sn profile use <name>");
    }
    write_response(global, &out)
}

/// `sn profile use` — repoint `default_profile`.
///
/// One file, but still a read → modify → write: without the lock it would
/// happily write back a `config.toml` it read before a concurrent
/// `sn profile add` landed, deleting that profile.
fn set_default(global: &GlobalFlags, name: String) -> Result<()> {
    config::with_config_lock(|| {
        let cfg_path = config_path()?;
        let mut cfg = load_config_from(&cfg_path)?;
        if !cfg.profiles.contains_key(&name) {
            return Err(Error::Usage(format!("profile '{name}' not found")));
        }
        cfg.default_profile = Some(name.clone());
        save_config_to(&cfg_path, &cfg)
    })?;
    write_response(
        global,
        &json!({ "ok": true, "profile": name, "default": true }),
    )
}

#[cfg(test)]
mod tests {
    use super::normalize_instance;

    #[test]
    fn short_name_gets_service_now_suffix() {
        assert_eq!(normalize_instance("dev380385"), "dev380385.service-now.com");
    }

    #[test]
    fn fqdn_passes_through() {
        assert_eq!(
            normalize_instance("acme.service-now.com"),
            "acme.service-now.com"
        );
    }

    #[test]
    fn full_url_passes_through() {
        assert_eq!(
            normalize_instance("https://dev380385.service-now.com"),
            "https://dev380385.service-now.com"
        );
    }

    #[test]
    fn http_url_passes_through() {
        assert_eq!(
            normalize_instance("http://localhost:8080"),
            "http://localhost:8080"
        );
    }

    #[test]
    fn trailing_slash_stripped() {
        assert_eq!(
            normalize_instance("dev380385/"),
            "dev380385.service-now.com"
        );
        assert_eq!(
            normalize_instance("https://dev380385.service-now.com/"),
            "https://dev380385.service-now.com"
        );
    }

    #[test]
    fn whitespace_trimmed() {
        assert_eq!(
            normalize_instance("  dev380385 \n"),
            "dev380385.service-now.com"
        );
    }

    #[test]
    fn empty_stays_empty() {
        // Guard the caller's `is_empty()` check: whitespace must not become the
        // bare suffix ".service-now.com".
        assert_eq!(normalize_instance("   "), "");
    }
}
