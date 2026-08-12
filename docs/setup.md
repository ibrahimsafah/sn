# Installation and setup

This guide takes you from nothing to a working, authenticated `sn`. If you just want the
short version: install with Homebrew, run `sn init`, answer the prompts, done.

- [Installation](#installation)
- [First-time setup](#first-time-setup)
- [Profiles](#profiles)
- [Non-interactive setup (CI, containers, agents)](#non-interactive-setup-ci-containers-agents)
- [OAuth / SSO](#oauth--sso)
- [Configuration files](#configuration-files)
- [Environment variables](#environment-variables)
- [Proxy and TLS](#proxy-and-tls)

## Installation

### Homebrew (macOS / Linux)

```bash
brew install tehubersheezy/sn/sn
# or: brew tap tehubersheezy/sn && brew install sn   (upgrade later with: brew upgrade sn)
```

### Shell installer (macOS / Linux)

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/tehubersheezy/servicenow-cli/releases/latest/download/sn-installer.sh | sh
```

### Windows (MSI or PowerShell)

Download `sn-x86_64-pc-windows-msvc.msi` (64-bit Intel/AMD) or `sn-aarch64-pc-windows-msvc.msi` (ARM64 — Surface Pro X, Copilot+ PCs) from the [latest release](https://github.com/tehubersheezy/servicenow-cli/releases/latest) and double-click. For unattended/SCCM/Intune deployment use `msiexec /i sn-x86_64-pc-windows-msvc.msi /qn`. Or install via PowerShell:

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/tehubersheezy/servicenow-cli/releases/latest/download/sn-installer.ps1 | iex"
```

### Pre-built binaries

Download from [Releases](https://github.com/tehubersheezy/servicenow-cli/releases): Linux (x86_64, ARM64), macOS (Intel, Apple Silicon), and Windows (x86_64, ARM64) — the latter as a portable `.zip` (no install) or `.msi` installer.

## First-time setup

`sn` supports **basic auth** (username + password) for most instances, and **OAuth / SSO**
for instances fronted by an external identity provider (Okta, Azure AD, ADFS), where the
password lives in the IdP and basic auth cannot work.

For basic auth, run `sn init` and answer the prompts:

```bash
sn init
# Profile name [default]:
# Instance (e.g. 'dev380385' or 'https://acme.service-now.com'): mycompany.service-now.com
# Auth method (basic/oauth) [basic]:
# Username: admin
# Password: ********
# profile 'default' saved and verified (mycompany.service-now.com).
# 'default' is now the default profile.
```

`sn init` checks the credentials against the instance before saving anything, so a typo'd
password fails here rather than on your fifth command. Verify the connection any time with
`sn ping`.

## Profiles

A **profile** is a saved identity: instance + credentials + any proxy/TLS settings, under a
name. `sn init` creates one *and makes it the default* — every command uses the default
profile unless told otherwise.

To add more instances without disturbing your default, use `sn profile add`:

```bash
sn profile add prod --instance prod.service-now.com --username svc-user --auth basic   # prompts for the password
sn --profile prod table list incident --setlimit 5    # -p prod also works
sn profile use prod                  # make it the default, when you're ready
```

(Omit `--auth basic` and it prompts for the auth method too. Any field you don't pass, it
asks for — on a terminal. Off one, it fails naming the flag instead. See below.)

Profile selection is `--profile NAME` (`-p NAME`) > `default_profile` > a clear error.
There are no per-field overrides — no env var or flag substitutes a different password into
an existing profile. Change identity by rewriting the profile or selecting a different one.

## Non-interactive setup (CI, containers, agents)

`sn profile add` is built to be scripted. It never prompts when stdin isn't a terminal — it
fails naming the flag it needed — so it cannot hang a pipeline. Pipe the password in rather
than passing `--password`, which is visible in `ps` output and shell history:

```bash
sn profile add ci --instance acme.service-now.com --username svc-user --password-stdin < secret.txt
# → {"auth":"basic","default":false,"instance":"acme.service-now.com","next":"sn profile use ci",
#    "ok":true,"profile":"ci","user":"svc-user","verified":true}
```

Keys come back sorted. `user` is the identity the instance resolved the credentials to — worth
asserting on in CI, since it catches a service account being silently swapped out.

It always checks the credentials against the instance, and **a profile that fails the check is
not written at all** — no half-configured identity to trip over later. Pass `--no-verify` to
register a profile without touching the network (air-gapped provisioning, or config management
that runs before the instance is reachable).

`"next"` appears only when there's something to do about it — above, that no default profile is
selected yet, so `ci` needs `sn profile use ci` or an explicit `--profile ci`.

`add` creates; it will not silently overwrite an identity you or a teammate may be relying on:

| | |
|---|---|
| profile already exists | exit 1 — pass `--force` to overwrite |
| required flag missing, no TTY | exit 1, naming the flag |
| credentials rejected | exit 4, nothing written |
| `--non-interactive` | never prompt, even on a terminal — fail naming the flag |
| `--set-default` | also make it the default (otherwise `add` leaves it alone) |

## OAuth / SSO

Configure the profile with `sn init --auth oauth` (or `sn profile add --auth oauth`), then run the
flow with `sn auth login`:

```bash
# Authorization-code + PKCE (default): a PUBLIC client — no secret needed or prompted for.
sn init --profile sso --auth oauth --instance acme.service-now.com --client-id <id>

# Non-interactive server-to-server: client_credentials is a CONFIDENTIAL client and needs a secret
# (prompted if --client-secret is omitted; --client-secret-stdin keeps it out of `ps`).
sn profile add svc --auth oauth --instance acme.service-now.com \
  --grant client_credentials --client-id <id> --client-secret-stdin < secret.txt

sn --profile sso auth login          # run the OAuth flow, cache tokens
```

The two grants differ in whether they can be set up headlessly. `client_credentials` mints a token
without a browser, so `sn profile add` verifies it like any other credential. `authorization_code`
**requires** a browser, so there is nothing for `sn profile add` to test on a machine that has none:
it refuses rather than save an untested profile. Pass `--no-verify` to register it anyway, then have
a human run `sn auth login`.

**One-time admin setup** (if the instance has no registry entry yet): **System OAuth → Application Registry → New → "Create an OAuth API endpoint for external clients"**; set the redirect URL to `http://localhost:8400/callback` — which must match `--redirect-uri` **exactly** — and copy the client ID. For the default authorization-code flow, enable **Public Client / PKCE required** so no secret is needed; only `client_credentials` needs the generated secret.

After login, tokens refresh transparently. Manage the session with `sn auth status` (method + token expiry), `sn auth refresh`, and `sn auth logout`. The client ID and redirect URI live in `config.toml`; the secret and tokens in `credentials.toml` (chmod 600).

Verify either auth method at any time with `sn ping`.

## Configuration files

Credentials use a two-file, AWS CLI-style split:

| File | Contains | Location (Linux) |
|---|---|---|
| `config.toml` | Instance URLs, default profile, non-secret OAuth config | `~/.config/sn/` |
| `credentials.toml` | Usernames, passwords, secrets, cached tokens (chmod 600) | `~/.config/sn/` |

macOS uses `~/Library/Application Support/sn/` and Windows `%APPDATA%\sn\`.

Point `sn` at a different config directory (for testing or sandboxing) with `SN_CONFIG_DIR`.

## Environment variables

| Env var | Description |
|---|---|
| `SN_CONFIG_DIR` | Override the config directory. Points **directly** at the folder holding `config.toml` and `credentials.toml` (no `sn` subdirectory appended). Cross-platform; when unset, the platform-native location is used. |
| `SN_PROXY` | HTTP/HTTPS/SOCKS5 proxy URL |
| `SN_NO_PROXY` | Comma-separated hosts to bypass the proxy |
| `SN_INSECURE=1` | Disable TLS certificate verification |
| `SN_CA_CERT` | Path to a custom CA cert for ServiceNow |
| `SN_PROXY_CA_CERT` | Path to a custom CA cert for the proxy |

```bash
SN_PROXY=http://proxy:8080 sn table list incident
SN_INSECURE=1 sn table list incident    # skip cert verification
```

There are deliberately no environment variables for credential values or profile selection — use profiles (`sn init`, `sn profile add`, `--profile`) instead. To keep a secret off the command line in a script, pipe it in with `sn profile add --password-stdin` / `--client-secret-stdin`.

## Proxy and TLS

Route through a proxy or adjust TLS per invocation:

```bash
sn --proxy http://proxy.corp:8080 table list incident   # also socks5://proxy:1080
sn --no-proxy table list incident                        # bypass a configured proxy for one call
sn --insecure table list incident                        # skip cert verification (dev/self-signed certs)
sn --ca-cert /path/to/ca.pem table list incident         # custom CA certificate
```

Any of these can live in a profile — non-secrets in `config.toml`, proxy credentials in `credentials.toml`:

```toml
# config.toml
[profiles.dev]
instance = "dev.example.com"
proxy = "http://proxy.corp:8080"
no_proxy = "localhost,127.0.0.1"
insecure = false
ca_cert = "/etc/ssl/custom-ca.pem"
proxy_ca_cert = "/etc/ssl/proxy-ca.pem"

# credentials.toml
[profiles.dev]
proxy_username = "proxy-user"
proxy_password = "proxy-pass"
```

Precedence for every proxy/TLS setting: CLI flag > env var (`SN_PROXY`, `SN_INSECURE=1`, …) > profile config.

`--insecure` is the exception: it is a logical OR across all three sources, not a chain. TLS verification is disabled if **any** of the flag, `SN_INSECURE`, or the profile's `insecure = true` says so — there is no way to turn it back *on* for one invocation of a profile that has it set. That's deliberate (a footgun should not be quietly re-armed by a stale config), but it means the only way to undo `insecure = true` is to edit the profile.
