use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    env,
    ffi::OsString,
    fs,
    hash::{DefaultHasher, Hash, Hasher},
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

    #[display("Failed to resolve current executable path: {_0}")]
    #[diagnostic(code(pacquet_executor::current_exe))]
    CurrentExe(#[error(source)] std::io::Error),

    #[display("Failed to prepare pnpm shim: {_0}")]
    #[diagnostic(code(pacquet_executor::pnpm_shim))]
    PnpmShim(#[error(source)] std::io::Error),
}

/// Parameters used to run one lifecycle script in a package.
pub struct ExecuteLifecycleScript<'a> {
    pub pkg_root: &'a Path,
    pub package_json_path: &'a Path,
    pub script_name: &'a str,
    pub script: &'a str,
    pub args: &'a [String],
    pub extra_env: &'a [(OsString, OsString)],
    pub script_shell: Option<&'a str>,
    pub shell_emulator: bool,
    pub init_cwd: &'a Path,
}

/// Parameters used to run one arbitrary command in a package context.
pub struct ExecuteCommand<'a> {
    pub pkg_root: &'a Path,
    pub current_dir: Option<&'a Path>,
    pub program: &'a str,
    pub args: &'a [String],
    pub extra_env: &'a [(OsString, OsString)],
    pub shell_mode: bool,
}

#[derive(Debug, Default, Clone)]
pub struct LifecycleScriptOutput {
    pub stdout: String,
    pub stderr: String,
}

/// Execute one script through a shell with pnpm-like script environment.
pub fn execute_lifecycle_script(opts: ExecuteLifecycleScript<'_>) -> Result<(), ExecutorError> {
    let command_line = append_args_to_script(opts.script, opts.args);
    let common_env = build_common_env(&opts)?;

    if opts.shell_emulator {
        execute_with_shell_emulator(opts.pkg_root, &command_line, &common_env, false)?;
        return Ok(());
    }

    if opts.script_shell.is_none()
        && let Some(mut command) =
            try_build_direct_lifecycle_command_windows(&command_line, &common_env)
    {
        apply_common_env(&mut command, opts.pkg_root, &common_env);
        return wait_for_command(&mut command);
    }

    let mut command = shell_command(opts.script_shell, &command_line)?;
    apply_common_env(&mut command, opts.pkg_root, &common_env);
    wait_for_command(&mut command)
}

/// Execute one arbitrary command with pnpm-like PATH preparation.
pub fn execute_command(opts: ExecuteCommand<'_>) -> Result<(), ExecutorError> {
    let path_var = prepend_node_modules_bin_paths(opts.pkg_root)?;
    let common_env = [(OsString::from("PATH"), path_var.clone())];
    let mut command = if opts.shell_mode {
        shell_command(None, &append_args_to_script(opts.program, opts.args))?
    } else {
        build_program_command(opts.program, opts.args, &common_env)
    };
    command.current_dir(opts.current_dir.unwrap_or(opts.pkg_root));
    command.env("PATH", path_var);
    for (key, value) in opts.extra_env {
        command.env(key, value);
    }
    wait_for_command(&mut command)
}

/// Execute one arbitrary command and capture its output.
pub fn execute_command_capture(
    opts: ExecuteCommand<'_>,
) -> Result<LifecycleScriptOutput, ExecutorError> {
    let path_var = prepend_node_modules_bin_paths(opts.pkg_root)?;
    let common_env = [(OsString::from("PATH"), path_var.clone())];
    let mut command = if opts.shell_mode {
        shell_command(None, &append_args_to_script(opts.program, opts.args))?
    } else {
        build_program_command(opts.program, opts.args, &common_env)
    };
    command.current_dir(opts.current_dir.unwrap_or(opts.pkg_root));
    command.env("PATH", path_var);
    for (key, value) in opts.extra_env {
        command.env(key, value);
    }
    wait_for_command_capture(&mut command)
}

/// Execute one script and capture its output instead of writing directly to stdio.
pub fn execute_lifecycle_script_capture(
    opts: ExecuteLifecycleScript<'_>,
) -> Result<LifecycleScriptOutput, ExecutorError> {
    let command_line = append_args_to_script(opts.script, opts.args);
    let common_env = build_common_env(&opts)?;

    if opts.shell_emulator {
        return execute_with_shell_emulator(opts.pkg_root, &command_line, &common_env, true);
    }

    if opts.script_shell.is_none()
        && let Some(mut command) =
            try_build_direct_lifecycle_command_windows(&command_line, &common_env)
    {
        apply_common_env(&mut command, opts.pkg_root, &common_env);
        return wait_for_command_capture(&mut command);
    }

    let mut command = shell_command(opts.script_shell, &command_line)?;
    apply_common_env(&mut command, opts.pkg_root, &common_env);
    wait_for_command_capture(&mut command)
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
    let mut bins = vec![prepare_pnpm_shim_dir()?];
    bins.extend(collect_node_modules_bin_paths(pkg_root));
    if let Some(current_path) = env::var_os("PATH") {
        bins.extend(env::split_paths(&current_path));
    }
    env::join_paths(bins).map_err(ExecutorError::JoinPaths)
}

fn prepare_pnpm_shim_dir() -> Result<PathBuf, ExecutorError> {
    let pacquet_bin = env::current_exe().map_err(ExecutorError::CurrentExe)?;

    let mut hasher = DefaultHasher::new();
    pacquet_bin.hash(&mut hasher);
    let hash = hasher.finish();
    let shim_dir = env::temp_dir().join("pacquet-pnpm-shim").join(format!("{hash:016x}"));
    fs::create_dir_all(&shim_dir).map_err(ExecutorError::PnpmShim)?;

    if cfg!(windows) {
        create_windows_pnpm_shims(&shim_dir, &pacquet_bin)?;
    } else {
        create_unix_pnpm_shim(&shim_dir, &pacquet_bin)?;
    }
    Ok(shim_dir)
}

fn create_windows_pnpm_shims(shim_dir: &Path, pacquet_bin: &Path) -> Result<(), ExecutorError> {
    let pacquet = pacquet_bin.to_string_lossy();
    let cmd_content = format!("@echo off\r\n\"{pacquet}\" %*\r\n");
    fs::write(shim_dir.join("pnpm.cmd"), cmd_content).map_err(ExecutorError::PnpmShim)?;

    let ps_path = pacquet.replace('\'', "''");
    let ps_content = format!("& '{ps_path}' @args\r\n");
    fs::write(shim_dir.join("pnpm.ps1"), ps_content).map_err(ExecutorError::PnpmShim)?;
    Ok(())
}

fn create_unix_pnpm_shim(shim_dir: &Path, pacquet_bin: &Path) -> Result<(), ExecutorError> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let pacquet = shell_quote_posix(&pacquet_bin.to_string_lossy());
    let content = format!("#!/bin/sh\nexec {pacquet} \"$@\"\n");
    let script_path = shim_dir.join("pnpm");
    fs::write(&script_path, content).map_err(ExecutorError::PnpmShim)?;
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&script_path).map_err(ExecutorError::PnpmShim)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).map_err(ExecutorError::PnpmShim)?;
    }
    Ok(())
}

fn build_common_env(
    opts: &ExecuteLifecycleScript<'_>,
) -> Result<Vec<(OsString, OsString)>, ExecutorError> {
    let path_var = prepend_node_modules_bin_paths(opts.pkg_root)?;
    let mut env = vec![
        (OsString::from("PATH"), path_var),
        (OsString::from("INIT_CWD"), opts.init_cwd.as_os_str().to_os_string()),
        (OsString::from("PNPM_SCRIPT_SRC_DIR"), opts.pkg_root.as_os_str().to_os_string()),
        (OsString::from("npm_lifecycle_event"), OsString::from(opts.script_name)),
        (OsString::from("npm_lifecycle_script"), OsString::from(opts.script)),
        (OsString::from("npm_package_json"), opts.package_json_path.as_os_str().to_os_string()),
        (OsString::from("npm_command"), OsString::from("run-script")),
    ];
    env.extend_from_slice(opts.extra_env);
    Ok(env)
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

fn wait_for_command_capture(command: &mut Command) -> Result<LifecycleScriptOutput, ExecutorError> {
    let output = command.output().map_err(ExecutorError::SpawnCommand)?;
    if !output.status.success() {
        return Err(ExecutorError::ExitStatus { code: output.status.code() });
    }

    Ok(LifecycleScriptOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn execute_with_shell_emulator(
    pkg_root: &Path,
    command_line: &str,
    common_env: &[(OsString, OsString)],
    capture: bool,
) -> Result<LifecycleScriptOutput, ExecutorError> {
    let mut output = LifecycleScriptOutput::default();
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

        let mut command = build_program_command(&argv[0], &argv[1..], common_env);
        apply_common_env(&mut command, pkg_root, common_env);
        for (key, value) in assignments {
            command.env(key, value);
        }
        if capture {
            let command_output = wait_for_command_capture(&mut command)?;
            output.stdout.push_str(&command_output.stdout);
            output.stderr.push_str(&command_output.stderr);
        } else {
            wait_for_command(&mut command)?;
        }
    }
    Ok(output)
}

fn build_program_command(
    program: &str,
    args: &[String],
    common_env: &[(OsString, OsString)],
) -> Command {
    if !cfg!(windows) {
        let mut command = Command::new(program);
        command.args(args);
        return command;
    }

    match resolve_program_for_emulator_windows(program, common_env) {
        WindowsResolvedProgram::Direct(target) => {
            let mut command = Command::new(target);
            command.args(args);
            command
        }
        WindowsResolvedProgram::PowerShellScript(target) => {
            let mut command = Command::new("powershell.exe");
            command
                .args(["-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(target)
                .args(args);
            command
        }
        WindowsResolvedProgram::BatchScript(target) => {
            let mut command = Command::new("cmd");
            command.args(["/d", "/c"]).arg(build_windows_batch_command_line(&target, args));
            command
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WindowsResolvedProgram {
    Direct(OsString),
    PowerShellScript(PathBuf),
    BatchScript(PathBuf),
}

fn resolve_program_for_emulator_windows(
    program: &str,
    common_env: &[(OsString, OsString)],
) -> WindowsResolvedProgram {
    let program_path = Path::new(program);
    if program_path.is_absolute() || program.contains('\\') || program.contains('/') {
        return classify_windows_program_target(PathBuf::from(program), OsString::from(program));
    }

    let path_var = common_env
        .iter()
        .find_map(|(key, value)| {
            key.to_string_lossy().eq_ignore_ascii_case("PATH").then_some(value.clone())
        })
        .or_else(|| env::var_os("PATH"));
    let Some(path_var) = path_var else {
        return WindowsResolvedProgram::Direct(OsString::from(program));
    };

    let has_extension = program_path.extension().is_some();
    let pathext = windows_candidate_extensions(has_extension);

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
                return classify_windows_program_target(
                    candidate.clone(),
                    candidate.into_os_string(),
                );
            }
        }
    }

    WindowsResolvedProgram::Direct(OsString::from(program))
}

fn try_build_direct_lifecycle_command_windows(
    command_line: &str,
    common_env: &[(OsString, OsString)],
) -> Option<Command> {
    if !cfg!(windows) || contains_windows_shell_metacharacters(command_line) {
        return None;
    }

    let tokens = shlex::split(command_line.trim())?;
    let (assignments, argv) = split_env_assignments(&tokens);
    if !assignments.is_empty() || argv.is_empty() {
        return None;
    }

    Some(build_program_command(&argv[0], &argv[1..], common_env))
}

fn contains_windows_shell_metacharacters(command_line: &str) -> bool {
    command_line.chars().any(|ch| matches!(ch, '|' | '>' | '<' | '&' | '(' | ')' | '%' | '!'))
}

fn build_windows_batch_command_line(target: &Path, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(shell_quote_windows_cmd(&target.to_string_lossy()));
    parts.extend(args.iter().map(|arg| shell_quote_windows_cmd(arg)));
    format!("call {}", parts.join(" "))
}

fn shell_quote_windows_cmd(input: &str) -> String {
    serde_json::to_string(input).expect("serialize Windows cmd arg to JSON string")
}

fn windows_candidate_extensions(has_extension: bool) -> Vec<String> {
    if has_extension {
        return vec![String::new()];
    }

    let mut extensions = vec![
        ".ps1".to_string(),
        ".exe".to_string(),
        ".com".to_string(),
        ".cmd".to_string(),
        ".bat".to_string(),
    ];
    if let Ok(pathext) = env::var("PATHEXT") {
        for ext in pathext.split(';').filter(|ext| !ext.is_empty()) {
            let ext = ext.to_ascii_lowercase();
            if !extensions.iter().any(|existing| existing.eq_ignore_ascii_case(&ext)) {
                extensions.push(ext);
            }
        }
    }
    extensions
}

fn classify_windows_program_target(
    candidate: PathBuf,
    fallback: OsString,
) -> WindowsResolvedProgram {
    let Some(extension) = candidate.extension().and_then(|ext| ext.to_str()) else {
        return WindowsResolvedProgram::Direct(fallback);
    };
    if extension.eq_ignore_ascii_case("ps1") {
        return WindowsResolvedProgram::PowerShellScript(candidate);
    }
    if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") {
        return WindowsResolvedProgram::BatchScript(candidate);
    }
    WindowsResolvedProgram::Direct(fallback)
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
    use tempfile::tempdir;

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

    #[test]
    fn build_common_env_appends_extra_env_entries() {
        let dir = tempdir().expect("tempdir");
        let package_json_path = dir.path().join("package.json");
        fs::write(&package_json_path, "{}").expect("write package.json");
        let extra_env =
            vec![(OsString::from("NODE_PATH"), OsString::from("/tmp/node_modules_extra"))];
        let opts = ExecuteLifecycleScript {
            pkg_root: dir.path(),
            package_json_path: &package_json_path,
            script_name: "build",
            script: "echo hi",
            args: &[],
            extra_env: &extra_env,
            script_shell: None,
            shell_emulator: false,
            init_cwd: dir.path(),
        };

        let common_env = build_common_env(&opts).expect("build common env");
        assert!(common_env.iter().any(|(key, value)| {
            key == OsStr::new("NODE_PATH") && value == OsStr::new("/tmp/node_modules_extra")
        }));
    }

    #[test]
    fn execute_command_runs_binary_from_node_modules_bin() {
        let dir = tempdir().expect("tempdir");
        let bin_dir = dir.path().join("node_modules/.bin");
        fs::create_dir_all(&bin_dir).expect("create .bin");

        #[cfg(windows)]
        {
            fs::write(bin_dir.join("hello.ps1"), "Set-Content -Path exec-result.txt -Value ok\r\n")
                .expect("write .ps1");
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let script_path = bin_dir.join("hello");
            fs::write(&script_path, "#!/bin/sh\necho ok > exec-result.txt\n")
                .expect("write script");
            let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script_path, permissions).expect("chmod");
        }

        execute_command(ExecuteCommand {
            pkg_root: dir.path(),
            current_dir: None,
            program: "hello",
            args: &[],
            extra_env: &[],
            shell_mode: false,
        })
        .expect("execute command");

        let result = fs::read_to_string(dir.path().join("exec-result.txt")).expect("read result");
        assert_eq!(result.trim(), "ok");
    }

    #[cfg(windows)]
    #[test]
    fn windows_resolution_prefers_powershell_shim_over_cmd() {
        let dir = tempdir().expect("tempdir");
        let bin_dir = dir.path().join("node_modules/.bin");
        fs::create_dir_all(&bin_dir).expect("create .bin");
        fs::write(bin_dir.join("vite.cmd"), "@echo off\r\n").expect("write .cmd");
        fs::write(bin_dir.join("vite.ps1"), "& 'vite.js' @args\r\n").expect("write .ps1");

        let resolved = resolve_program_for_emulator_windows(
            "vite",
            &[(OsString::from("PATH"), bin_dir.as_os_str().to_os_string())],
        );

        match resolved {
            WindowsResolvedProgram::PowerShellScript(path) => {
                assert_eq!(path.file_name().and_then(|name| name.to_str()), Some("vite.ps1"));
            }
            other => panic!("expected PowerShell script target, got {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn simple_windows_lifecycle_command_bypasses_cmd_shell() {
        let dir = tempdir().expect("tempdir");
        let bin_dir = dir.path().join("node_modules/.bin");
        fs::create_dir_all(&bin_dir).expect("create .bin");
        fs::write(bin_dir.join("vite.ps1"), "& 'vite.js' @args\r\n").expect("write .ps1");

        let command = try_build_direct_lifecycle_command_windows(
            "vite --host",
            &[(OsString::from("PATH"), bin_dir.as_os_str().to_os_string())],
        )
        .expect("build direct lifecycle command");

        assert_eq!(command.get_program(), OsStr::new("powershell.exe"));
        let args =
            command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-File".to_string(),
                bin_dir.join("vite.ps1").to_string_lossy().into_owned(),
                "--host".to_string(),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_lifecycle_command_with_shell_syntax_keeps_shell_path() {
        let dir = tempdir().expect("tempdir");
        let bin_dir = dir.path().join("node_modules/.bin");
        fs::create_dir_all(&bin_dir).expect("create .bin");
        fs::write(bin_dir.join("vite.ps1"), "& 'vite.js' @args\r\n").expect("write .ps1");

        assert!(
            try_build_direct_lifecycle_command_windows(
                "vite && echo done",
                &[(OsString::from("PATH"), bin_dir.as_os_str().to_os_string())],
            )
            .is_none()
        );
    }
}
