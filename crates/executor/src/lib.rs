use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum ExecutorError {
    #[display("Failed to spawn command: {_0}")]
    #[diagnostic(code(pacquet_executor::spawn_command))]
    SpawnCommand(#[error(source)] std::io::Error),

    #[display("Process exited with non-zero status code: {code:?}")]
    #[diagnostic(code(pacquet_executor::exit_status))]
    ExitStatus { code: Option<i32> },

    #[display("Failed to build PATH environment variable: {_0}")]
    #[diagnostic(code(pacquet_executor::join_paths))]
    JoinPaths(#[error(source)] env::JoinPathsError),

    #[display("Cannot spawn .bat or .cmd as script-shell on Windows: {_0}")]
    #[diagnostic(
        code(pacquet_executor::invalid_script_shell_windows),
        help("Use a .exe shell, or unset script-shell")
    )]
    InvalidScriptShellWindows(#[error(not(source))] String),
}

/// Parameters used to run one lifecycle script in a package.
pub struct ExecuteLifecycleScript<'a> {
    pub pkg_root: &'a Path,
    pub package_json_path: &'a Path,
    pub script_name: &'a str,
    pub script: &'a str,
    pub args: &'a [String],
    pub script_shell: Option<&'a str>,
    pub shell_emulator: bool,
    pub init_cwd: &'a Path,
}

/// Execute one script through a shell with pnpm-like script environment.
pub fn execute_lifecycle_script(opts: ExecuteLifecycleScript<'_>) -> Result<(), ExecutorError> {
    let command_line = append_args_to_script(opts.script, opts.args);
    let common_env = build_common_env(&opts)?;

    if opts.shell_emulator {
        return execute_with_shell_emulator(opts.pkg_root, &command_line, &common_env);
    }

    let mut command = shell_command(opts.script_shell, &command_line)?;
    apply_common_env(&mut command, opts.pkg_root, &common_env);
    wait_for_command(&mut command)
}

fn shell_command(script_shell: Option<&str>, command_line: &str) -> Result<Command, ExecutorError> {
    if cfg!(windows) {
        return shell_command_windows(script_shell, command_line);
    }
    Ok(shell_command_unix(script_shell, command_line))
}

fn shell_command_unix(script_shell: Option<&str>, command_line: &str) -> Command {
    let shell = script_shell.unwrap_or("sh");
    let mut command = Command::new(shell);
    command.arg("-c").arg(command_line);
    command
}

fn shell_command_windows(
    script_shell: Option<&str>,
    command_line: &str,
) -> Result<Command, ExecutorError> {
    let Some(shell) = script_shell else {
        let mut command = Command::new("cmd");
        command.args(["/d", "/s", "/c"]).arg(command_line);
        return Ok(command);
    };

    if is_windows_batch_file(shell) {
        return Err(ExecutorError::InvalidScriptShellWindows(shell.to_string()));
    }

    let lower = shell.to_ascii_lowercase();
    let mut command = Command::new(shell);
    if lower.ends_with("cmd") || lower.ends_with("cmd.exe") {
        command.args(["/d", "/s", "/c"]).arg(command_line);
    } else if lower.ends_with("powershell")
        || lower.ends_with("powershell.exe")
        || lower.ends_with("pwsh")
        || lower.ends_with("pwsh.exe")
    {
        command.args(["-NoLogo", "-NoProfile", "-Command"]).arg(command_line);
    } else {
        command.arg("-c").arg(command_line);
    }
    Ok(command)
}

fn is_windows_batch_file(script_shell: &str) -> bool {
    if !cfg!(windows) {
        return false;
    }
    let shell_lower = script_shell.to_ascii_lowercase();
    shell_lower.ends_with(".bat") || shell_lower.ends_with(".cmd")
}

fn prepend_node_modules_bin_paths(pkg_root: &Path) -> Result<OsString, ExecutorError> {
    let mut bins = collect_node_modules_bin_paths(pkg_root);
    if let Some(current_path) = env::var_os("PATH") {
        bins.extend(env::split_paths(&current_path));
    }
    env::join_paths(bins).map_err(ExecutorError::JoinPaths)
}

fn build_common_env(
    opts: &ExecuteLifecycleScript<'_>,
) -> Result<Vec<(OsString, OsString)>, ExecutorError> {
    let path_var = prepend_node_modules_bin_paths(opts.pkg_root)?;
    Ok(vec![
        (OsString::from("PATH"), path_var),
        (OsString::from("INIT_CWD"), opts.init_cwd.as_os_str().to_os_string()),
        (OsString::from("PNPM_SCRIPT_SRC_DIR"), opts.pkg_root.as_os_str().to_os_string()),
        (OsString::from("npm_lifecycle_event"), OsString::from(opts.script_name)),
        (OsString::from("npm_lifecycle_script"), OsString::from(opts.script)),
        (OsString::from("npm_package_json"), opts.package_json_path.as_os_str().to_os_string()),
        (OsString::from("npm_command"), OsString::from("run-script")),
    ])
}

fn apply_common_env(command: &mut Command, pkg_root: &Path, common_env: &[(OsString, OsString)]) {
    command.current_dir(pkg_root);
    for (key, value) in common_env {
        command.env(key, value);
    }
}

fn wait_for_command(command: &mut Command) -> Result<(), ExecutorError> {
    let status = command.status().map_err(ExecutorError::SpawnCommand)?;
    if status.success() { Ok(()) } else { Err(ExecutorError::ExitStatus { code: status.code() }) }
}

fn execute_with_shell_emulator(
    pkg_root: &Path,
    command_line: &str,
    common_env: &[(OsString, OsString)],
) -> Result<(), ExecutorError> {
    let command_segments = split_by_double_and(command_line);
    for segment in command_segments {
        let tokens = shlex::split(segment.trim()).ok_or_else(|| {
            ExecutorError::SpawnCommand(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Unable to parse script segment: {segment}"),
            ))
        })?;
        if tokens.is_empty() {
            continue;
        }
        let (assignments, argv) = split_env_assignments(&tokens);
        if argv.is_empty() {
            continue;
        }

        let mut command = Command::new(resolve_program_for_emulator(&argv[0], common_env));
        if argv.len() > 1 {
            command.args(&argv[1..]);
        }
        apply_common_env(&mut command, pkg_root, common_env);
        for (key, value) in assignments {
            command.env(key, value);
        }
        wait_for_command(&mut command)?;
    }
    Ok(())
}

fn resolve_program_for_emulator(program: &str, common_env: &[(OsString, OsString)]) -> OsString {
    if !cfg!(windows) {
        return OsString::from(program);
    }
    resolve_program_for_emulator_windows(program, common_env)
}

fn resolve_program_for_emulator_windows(
    program: &str,
    common_env: &[(OsString, OsString)],
) -> OsString {
    let program_path = Path::new(program);
    if program_path.is_absolute() || program.contains('\\') || program.contains('/') {
        return OsString::from(program);
    }

    let path_var = common_env
        .iter()
        .find_map(|(key, value)| {
            key.to_string_lossy().eq_ignore_ascii_case("PATH").then_some(value.clone())
        })
        .or_else(|| env::var_os("PATH"));
    let Some(path_var) = path_var else {
        return OsString::from(program);
    };

    let has_extension = program_path.extension().is_some();
    let pathext = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let pathext = if has_extension {
        vec![String::new()]
    } else {
        pathext.split(';').filter(|ext| !ext.is_empty()).map(ToOwned::to_owned).collect::<Vec<_>>()
    };

    for dir in env::split_paths(&path_var) {
        let base = dir.join(program);
        for ext in &pathext {
            let candidate = if ext.is_empty() {
                base.clone()
            } else {
                let mut os = base.as_os_str().to_os_string();
                os.push(ext);
                PathBuf::from(os)
            };
            if candidate.is_file() {
                return candidate.into_os_string();
            }
        }
    }

    OsString::from(program)
}

fn split_by_double_and(command_line: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut chars = command_line.char_indices().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    while let Some((idx, ch)) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' if !in_single => {
                escaped = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            '&' if !in_single && !in_double => {
                if let Some((next_idx, '&')) = chars.peek().copied() {
                    parts.push(command_line[start..idx].trim());
                    let _ = chars.next();
                    start = next_idx + '&'.len_utf8();
                }
            }
            _ => {}
        }
    }
    parts.push(command_line[start..].trim());
    parts.into_iter().filter(|segment| !segment.is_empty()).collect()
}

fn split_env_assignments(tokens: &[String]) -> (Vec<(OsString, OsString)>, &[String]) {
    let mut assignments = Vec::new();
    let mut idx = 0usize;
    while idx < tokens.len() {
        let token = &tokens[idx];
        if let Some((key, value)) = split_env_assignment_token(token) {
            assignments.push((OsString::from(key), OsString::from(value)));
            idx += 1;
            continue;
        }
        break;
    }
    (assignments, &tokens[idx..])
}

fn split_env_assignment_token(token: &str) -> Option<(&str, &str)> {
    let (key, value) = token.split_once('=')?;
    if key.is_empty() {
        return None;
    }
    let mut key_chars = key.chars();
    let first = key_chars.next()?;
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return None;
    }
    if !key_chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        return None;
    }
    Some((key, value))
}

fn collect_node_modules_bin_paths(pkg_root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut current = Some(pkg_root);
    while let Some(dir) = current {
        let bin_dir = dir.join("node_modules").join(".bin");
        if bin_dir.exists() {
            paths.push(bin_dir);
        }
        current = dir.parent();
    }
    paths
}

fn append_args_to_script(script: &str, args: &[String]) -> String {
    if args.is_empty() {
        return script.to_string();
    }
    let escaped = if cfg!(windows) {
        args.iter()
            .map(|arg| serde_json::to_string(arg).expect("serialize arg to JSON string"))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        args.iter().map(|arg| shell_quote_posix(arg)).collect::<Vec<_>>().join(" ")
    };
    format!("{script} {escaped}")
}

fn shell_quote_posix(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }
    let is_safe = input.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '@' | '+' | '=')
    });
    if is_safe { input.to_string() } else { format!("'{}'", input.replace('\'', "'\"'\"'")) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn posix_quoting_keeps_safe_chars() {
        assert_eq!(shell_quote_posix("abc-1.2_/x"), "abc-1.2_/x");
    }

    #[test]
    fn posix_quoting_wraps_unsafe_chars() {
        assert_eq!(shell_quote_posix("hello world"), "'hello world'");
        assert_eq!(shell_quote_posix("a'b"), "'a'\"'\"'b'");
    }

    #[test]
    fn append_args_without_args_keeps_script() {
        assert_eq!(append_args_to_script("echo hi", &[]), "echo hi");
    }

    #[test]
    fn split_by_double_and_respects_quotes() {
        assert_eq!(split_by_double_and("a && b && c"), vec!["a", "b", "c"]);
        assert_eq!(split_by_double_and("echo 'a && b' && c"), vec!["echo 'a && b'", "c"]);
        assert_eq!(split_by_double_and("echo \"a && b\" && c"), vec!["echo \"a && b\"", "c"]);
    }

    #[test]
    fn split_env_assignments_extracts_prefix_vars() {
        let tokens =
            vec!["FOO=bar".to_string(), "X_2=1".to_string(), "echo".to_string(), "ok".to_string()];
        let (assignments, argv) = split_env_assignments(&tokens);
        assert_eq!(assignments.len(), 2);
        assert_eq!(assignments[0].0, OsStr::new("FOO"));
        assert_eq!(assignments[0].1, OsStr::new("bar"));
        assert_eq!(assignments[1].0, OsStr::new("X_2"));
        assert_eq!(argv, &["echo".to_string(), "ok".to_string()]);
    }

    #[test]
    fn split_env_assignment_rejects_invalid_keys() {
        assert_eq!(split_env_assignment_token("1A=b"), None);
        assert_eq!(split_env_assignment_token("-A=b"), None);
        assert_eq!(split_env_assignment_token("A"), None);
        assert_eq!(split_env_assignment_token("A=b"), Some(("A", "b")));
    }
}
