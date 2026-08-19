use crate::cli::GlobalFlags;
use crate::cli::profile::{
    Caller, ProfileAddArgs, SavePolicy, resolve_input, resolve_name, save_and_verify,
};
use crate::config::{AuthMethod, OAuthGrant};
use crate::error::Result;

#[derive(clap::Args, Debug)]
pub struct InitArgs {
    /// Profile name to create or update (default: "default").
    #[arg(long)]
    pub profile: Option<String>,
    /// Instance: short name (`dev380385`) or full URL.
    #[arg(long)]
    pub instance: Option<String>,
    /// Authentication method: `basic` (username/password), `oauth` (SSO /
    /// OAuth 2.0), or `apikey` (REST API key).
    #[arg(long, value_enum)]
    pub auth: Option<AuthMethod>,
    /// Username (basic auth only).
    #[arg(long)]
    pub username: Option<String>,
    /// Password (basic auth only). Convenience flag; prefer the interactive
    /// prompt — `--password` is visible in `ps` output and shell history.
    #[arg(long)]
    pub password: Option<String>,
    /// REST API key (apikey auth only). Convenience flag; prefer the
    /// interactive prompt — `--api-key` is visible in `ps` output and shell
    /// history.
    #[arg(long)]
    pub api_key: Option<String>,
    /// OAuth client_id (oauth only).
    #[arg(long)]
    pub client_id: Option<String>,
    /// OAuth client secret (oauth confidential clients).
    #[arg(long)]
    pub client_secret: Option<String>,
    /// OAuth loopback redirect URI (oauth only). Defaults to http://localhost:8400/callback.
    #[arg(long, value_name = "URL")]
    pub redirect_uri: Option<String>,
    /// OAuth grant: authorization_code (SSO, default) or client_credentials.
    #[arg(long, value_enum)]
    pub grant: Option<OAuthGrant>,
    /// Disable PKCE for the authorization-code flow.
    #[arg(long)]
    pub no_pkce: bool,
}

/// `sn init` — the first-run wizard: stand up a profile and make it the one
/// commands use.
///
/// It shares its whole implementation with `sn profile add` (see
/// `cli::profile`), and differs only in the three policies that make it an
/// onboarding command rather than a scripting one: it **always** claims
/// `default_profile`, it upserts rather than refusing to overwrite, and it
/// always verifies. Use `sn profile add` to register an additional profile
/// without disturbing the default.
pub fn run(global: &GlobalFlags, args: InitArgs) -> Result<()> {
    let add = ProfileAddArgs {
        name: args.profile,
        instance: args.instance,
        auth: args.auth,
        username: args.username,
        password: args.password,
        password_stdin: false,
        api_key: args.api_key,
        api_key_stdin: false,
        client_id: args.client_id,
        client_secret: args.client_secret,
        client_secret_stdin: false,
        redirect_uri: args.redirect_uri,
        grant: args.grant,
        no_pkce: args.no_pkce,
        force: true,
        no_verify: false,
        set_default: true,
        non_interactive: false,
    };

    // Unlike `sn profile add`, a nameless `sn init` is the documented way to set
    // up the first profile, so the name falls back to "default".
    // `Caller::Init` is what keeps a missing-field error from naming
    // `--password-stdin` / `--non-interactive`: flags the shared core has but
    // `sn init` does not accept.
    let name = resolve_name(&add, Some("default".into()), Caller::Init)?;
    let input = resolve_input(&add, name, Caller::Init)?;
    // `init` is the onboarding wizard: it always claims the default profile,
    // always verifies, and upserts rather than refusing an existing name.
    let user = save_and_verify(
        global,
        &input,
        SavePolicy {
            set_default: true,
            verify: true,
            refuse_existing: false,
        },
    )?;

    let (name, instance) = (&input.name, &input.instance);
    match input.auth {
        AuthMethod::Basic | AuthMethod::Apikey => {
            eprintln!("profile '{name}' saved and verified ({instance}).");
        }
        AuthMethod::Oauth => {
            let who = user.unwrap_or_else(|| "(unknown)".into());
            eprintln!(
                "profile '{name}' saved and authenticated via oauth ({instance}, user {who})."
            );
        }
    }
    eprintln!("'{name}' is now the default profile.");
    Ok(())
}
