use clap_complete::generate;
use std::io;

/// Shells supported by `sn completion`.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
    Elvish,
}

impl From<Shell> for clap_complete::Shell {
    fn from(s: Shell) -> Self {
        match s {
            Shell::Bash => clap_complete::Shell::Bash,
            Shell::Zsh => clap_complete::Shell::Zsh,
            Shell::Fish => clap_complete::Shell::Fish,
            Shell::Powershell => clap_complete::Shell::PowerShell,
            Shell::Elvish => clap_complete::Shell::Elvish,
        }
    }
}

#[derive(clap::Args, Debug)]
pub struct CompletionArgs {
    /// Shell to generate completions for.
    pub shell: Shell,
}

/// Emit a shell completion script for the chosen shell to stdout.
pub fn run(args: CompletionArgs) -> crate::error::Result<()> {
    let mut cmd = crate::cli::command();
    let shell: clap_complete::Shell = args.shell.into();
    let mut out = io::stdout().lock();
    generate(shell, &mut cmd, "sn", &mut out);
    Ok(())
}
