mod cli_args;
mod state;

use clap::Parser;
use cli_args::CliArgs;
use miette::set_panic_hook;
use pacquet_diagnostics::enable_tracing_by_env;
use pacquet_package_manifest::PackageManifest;
use state::State;
use std::{env, ffi::OsString, path::PathBuf};

const KNOWN_COMMANDS: &[&str] = &[
    "init",
    "add",
    "approve-builds",
    "audit",
    "cache",
    "cat-file",
    "cat-index",
    "help",
    "config",
    "c",
    "completion",
    "create",
    "deploy",
    "bin",
    "root",
    "install",
    "i",
    "link",
    "ln",
    "ci",
    "clean-install",
    "ic",
    "install-clean",
    "install-test",
    "it",
    "env",
    "dedupe",
    "dlx",
    "doctor",
    "exec",
    "fetch",
    "find-hash",
    "ignored-builds",
    "import",
    "rebuild",
    "rb",
    "remove",
    "rm",
    "uninstall",
    "un",
    "uni",
    "list",
    "ls",
    "la",
    "ll",
    "licenses",
    "outdated",
    "pack",
    "patch",
    "patch-commit",
    "patch-remove",
    "publish",
    "prune",
    "why",
    "unlink",
    "dislink",
    "get",
    "set",
    "restart",
    "recursive",
    "multi",
    "m",
    "test",
    "run",
    "run-script",
    "start",
    "setup",
    "self-update",
    "server",
    "store",
    "update",
    "up",
    "upgrade",
    // Registry & package management commands (natively implemented)
    "access",
    "adduser",
    "bugs",
    "deprecate",
    "dist-tag",
    "docs",
    "edit",
    "home",
    "info",
    "issues",
    "login",
    "logout",
    "owner",
    "ping",
    "prefix",
    "profile",
    "pkg",
    "repo",
    "s",
    "se",
    "search",
    "find",
    "set-script",
    "show",
    "star",
    "stars",
    "team",
    "token",
    "unpublish",
    "unstar",
    "version",
    "v",
    "view",
    "whoami",
    "xmas",
];

const FILTERABLE_COMMANDS: &[&str] = &[
    "add",
    "deploy",
    "exec",
    "install",
    "i",
    "outdated",
    "rebuild",
    "rb",
    "remove",
    "rm",
    "uninstall",
    "un",
    "uni",
    "run",
    "run-script",
    "unlink",
    "dislink",
    "update",
    "up",
    "upgrade",
];

pub async fn main() -> miette::Result<()> {
    enable_tracing_by_env();
    set_panic_hook();
    CliArgs::parse_from(preprocess_cli_args(env::args_os())).run().await
}

fn preprocess_cli_args(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut args = args.into_iter().collect::<Vec<_>>();
    let Some(mut command_index) = find_command_index(&args) else {
        return args;
    };

    let mut command = args[command_index].to_string_lossy().into_owned();
    if let Some(rewritten) = rewrite_recursive_command(&args, command_index, &command) {
        args = rewritten;
        command_index = find_command_index(&args).unwrap_or(command_index);
        command = args[command_index].to_string_lossy().into_owned();
    }
    args = rewrite_leading_filter_options(args, command_index, &command);
    command_index = find_command_index(&args).unwrap_or(command_index);
    command = args[command_index].to_string_lossy().into_owned();

    if KNOWN_COMMANDS.contains(&command.as_str()) {
        return args;
    }

    let Some(script_name) = fallback_script_name(&command) else {
        return args;
    };
    if !has_script_for_fallback(&args, command_index, script_name) {
        return args;
    }

    args[command_index] = OsString::from("run");
    args.insert(command_index + 1, OsString::from(script_name));
    args
}

fn find_command_index(args: &[OsString]) -> Option<usize> {
    let mut index = 1usize;
    while index < args.len() {
        let token = args[index].to_string_lossy();
        match token.as_ref() {
            "-C" | "--dir" => {
                index += 2;
            }
            "--filter" => {
                if index + 1 >= args.len() {
                    return None;
                }
                index += 2;
            }
            "-w" | "--workspace-root" | "-h" | "--help" | "-V" | "--version" => {
                index += 1;
            }
            "--" => return None,
            _ if token.starts_with("--dir=") => {
                index += 1;
            }
            _ if token.starts_with("--filter=") => {
                index += 1;
            }
            _ if token.starts_with('-') => return None,
            _ => return Some(index),
        }
    }
    None
}

fn fallback_script_name(command: &str) -> Option<&str> {
    match command {
        "t" | "tst" => Some("test"),
        "help" => None,
        other => Some(other),
    }
}

fn rewrite_recursive_command(
    args: &[OsString],
    command_index: usize,
    command: &str,
) -> Option<Vec<OsString>> {
    if !matches!(command, "recursive" | "multi" | "m") {
        return None;
    }

    let nested = args.get(command_index + 1)?.to_string_lossy().into_owned();
    let (target_command, extra_param) = match nested.as_str() {
        "run" => ("run", None),
        "test" => ("run", Some("test")),
        "exec" => ("exec", None),
        "install" | "i" => ("install", None),
        "rebuild" | "rb" => ("rebuild", None),
        "add" => ("add", None),
        "remove" | "rm" | "uninstall" | "un" | "uni" => ("remove", None),
        "unlink" | "dislink" => ("unlink", None),
        "outdated" => ("outdated", None),
        "update" | "up" | "upgrade" => ("update", None),
        _ => return None,
    };

    let mut rewritten = args.to_vec();
    rewritten[command_index] = OsString::from(target_command);
    rewritten.remove(command_index + 1);
    rewritten.insert(command_index + 1, OsString::from("--recursive"));
    if let Some(param) = extra_param {
        rewritten.insert(command_index + 2, OsString::from(param));
    }
    Some(rewritten)
}

fn rewrite_leading_filter_options(
    args: Vec<OsString>,
    command_index: usize,
    command: &str,
) -> Vec<OsString> {
    if !FILTERABLE_COMMANDS.contains(&command) {
        return args;
    }

    let mut rewritten = Vec::with_capacity(args.len());
    let mut leading_filters = Vec::<OsString>::new();
    let mut index = 0usize;
    while index < args.len() {
        if index < command_index {
            let token = args[index].to_string_lossy();
            if token == "--filter" && index + 1 < command_index {
                leading_filters.push(args[index].clone());
                leading_filters.push(args[index + 1].clone());
                index += 2;
                continue;
            }
            if token.starts_with("--filter=") {
                leading_filters.push(args[index].clone());
                index += 1;
                continue;
            }
        }
        rewritten.push(args[index].clone());
        index += 1;
    }

    if leading_filters.is_empty() {
        return rewritten;
    }

    let insert_at = find_command_index(&rewritten).map_or(1, |idx| idx + 1);
    rewritten.splice(insert_at..insert_at, leading_filters);
    rewritten
}

fn has_script_for_fallback(args: &[OsString], command_index: usize, script_name: &str) -> bool {
    let Some(package_dir) = parse_package_dir(args, command_index) else {
        return false;
    };
    let manifest_path = package_dir.join("package.json");
    if !manifest_path.is_file() {
        return false;
    }
    let Ok(manifest) = PackageManifest::from_path(manifest_path) else {
        return false;
    };
    manifest.script(script_name, true).ok().flatten().is_some()
}

fn parse_package_dir(args: &[OsString], command_index: usize) -> Option<PathBuf> {
    let current_dir = env::current_dir().ok()?;
    let mut dir = current_dir.clone();
    let mut workspace_root = false;
    let mut index = 1usize;
    while index < command_index {
        let token = args[index].to_string_lossy();
        match token.as_ref() {
            "-C" | "--dir" => {
                let value = args.get(index + 1)?.to_string_lossy().into_owned();
                dir = resolve_dir(&current_dir, &value);
                index += 2;
            }
            "-w" | "--workspace-root" => {
                workspace_root = true;
                index += 1;
            }
            _ if token.starts_with("--dir=") => {
                let value = token.trim_start_matches("--dir=");
                dir = resolve_dir(&current_dir, value);
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }
    if workspace_root { state::find_workspace_root(&dir) } else { Some(dir) }
}

fn resolve_dir(current_dir: &std::path::Path, value: &str) -> PathBuf {
    let candidate = PathBuf::from(value);
    if candidate.is_absolute() { candidate } else { current_dir.join(candidate) }
}
