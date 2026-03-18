use clap::{Args, ValueEnum};

const COMPLETION_COMMANDS: &[&str] = &[
    "init",
    "add",
    "approve-builds",
    "audit",
    "bin",
    "cache",
    "cat-file",
    "cat-index",
    "ci",
    "completion",
    "config",
    "create",
    "dedupe",
    "deploy",
    "dlx",
    "doctor",
    "env",
    "exec",
    "fetch",
    "find-hash",
    "help",
    "ignored-builds",
    "import",
    "install",
    "install-test",
    "licenses",
    "link",
    "list",
    "outdated",
    "pack",
    "patch",
    "patch-commit",
    "patch-remove",
    "publish",
    "prune",
    "rebuild",
    "recursive",
    "remove",
    "restart",
    "root",
    "run",
    "self-update",
    "server",
    "setup",
    "start",
    "store",
    "test",
    "unlink",
    "update",
    "why",
];

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Elvish,
    Fish,
    #[value(name = "powershell", alias = "power-shell")]
    PowerShell,
    Zsh,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    /// Shell to generate completions for.
    shell: CompletionShell,
}

impl CompletionArgs {
    pub fn run(self) -> miette::Result<()> {
        match self.shell {
            CompletionShell::Bash => {
                print!("{BASH_COMPLETION_PREFIX}{}{BASH_COMPLETION_SUFFIX}", completion_words())
            }
            CompletionShell::Elvish => {
                print!("{ELVISH_COMPLETION_PREFIX}{}{ELVISH_COMPLETION_SUFFIX}", completion_words())
            }
            CompletionShell::Fish => {
                print!("{FISH_COMPLETION_PREFIX}{}{FISH_COMPLETION_SUFFIX}", completion_words())
            }
            CompletionShell::PowerShell => {
                print!(
                    "{POWERSHELL_COMPLETION_PREFIX}{}{POWERSHELL_COMPLETION_SUFFIX}",
                    completion_words()
                )
            }
            CompletionShell::Zsh => {
                print!("{ZSH_COMPLETION_PREFIX}{}{ZSH_COMPLETION_SUFFIX}", completion_words())
            }
        }
        Ok(())
    }
}

fn completion_words() -> String {
    COMPLETION_COMMANDS.join(" ")
}

const BASH_COMPLETION_PREFIX: &str = r#"_pacquet_completions() {
  local cur="${COMP_WORDS[COMP_CWORD]}"
  COMPREPLY=( $(compgen -W ""#;

const BASH_COMPLETION_SUFFIX: &str = r#"" -- "$cur") )
}
complete -F _pacquet_completions pacquet
"#;

const ELVISH_COMPLETION_PREFIX: &str = r#"edit:completion:arg-completer[pacquet] = { |@words|
  put "#;

const ELVISH_COMPLETION_SUFFIX: &str = r#"
}
"#;

const FISH_COMPLETION_PREFIX: &str = r#"complete -c pacquet -f
for cmd in "#;

const FISH_COMPLETION_SUFFIX: &str = r#"
    complete -c pacquet -a $cmd
end
"#;

const POWERSHELL_COMPLETION_PREFIX: &str = r#"Register-ArgumentCompleter -Native -CommandName pacquet -ScriptBlock {
    param($wordToComplete, $commandAst, $cursorPosition)
    "#;

const POWERSHELL_COMPLETION_SUFFIX: &str = r#".Split(' ') | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
        [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
    }
}
"#;

const ZSH_COMPLETION_PREFIX: &str = r#"#compdef pacquet
local -a commands
commands=("#;

const ZSH_COMPLETION_SUFFIX: &str = r#")
_describe 'command' commands
"#;
