pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::{fs, path::Path, process::Command};

#[cfg(unix)]
fn write_unix_executable(path: &Path, content: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, content).expect("write executable");
    let mut permissions = fs::metadata(path).expect("read metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable permissions");
}

fn write_bin_success_script(workspace: &Path, name: &str) {
    let bin_dir = workspace.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).expect("create node_modules/.bin");

    #[cfg(windows)]
    {
        fs::write(bin_dir.join(format!("{name}.cmd")), "@echo off\r\necho ok> exec-result.txt\r\n")
            .expect("write .cmd file");
    }

    #[cfg(unix)]
    {
        write_unix_executable(&bin_dir.join(name), "#!/bin/sh\necho ok > exec-result.txt\n");
    }
}

fn write_bin_failure_script(workspace: &Path, name: &str) {
    let bin_dir = workspace.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).expect("create node_modules/.bin");

    #[cfg(windows)]
    {
        fs::write(bin_dir.join(format!("{name}.cmd")), "@echo off\r\nexit /b 7\r\n")
            .expect("write .cmd file");
    }

    #[cfg(unix)]
    {
        write_unix_executable(&bin_dir.join(name), "#!/bin/sh\nexit 7\n");
    }
}

fn write_bin_dump_env_script(workspace: &Path, name: &str) {
    let bin_dir = workspace.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).expect("create node_modules/.bin");

    #[cfg(windows)]
    {
        fs::write(
            bin_dir.join(format!("{name}.cmd")),
            "@echo off\r\necho %PNPM_PACKAGE_NAME%> exec-env.txt\r\necho %npm_command%>> exec-env.txt\r\n",
        )
        .expect("write .cmd file");
    }

    #[cfg(unix)]
    {
        write_unix_executable(
            &bin_dir.join(name),
            "#!/bin/sh\necho \"$PNPM_PACKAGE_NAME\" > exec-env.txt\necho \"$npm_command\" >> exec-env.txt\n",
        );
    }
}

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn exec_should_run_binary_from_node_modules_bin() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    write_bin_success_script(&workspace, "hello");

    pacquet_command(&workspace).with_args(["exec", "hello"]).assert().success();

    let result = fs::read_to_string(workspace.join("exec-result.txt")).expect("read exec result");
    assert_eq!(result.trim(), "ok");

    drop(root); // cleanup
}

#[test]
fn exec_should_work_without_package_json() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    write_bin_success_script(&workspace, "hello");

    pacquet_command(&workspace).with_args(["exec", "hello"]).assert().success();

    let result = fs::read_to_string(workspace.join("exec-result.txt")).expect("read exec result");
    assert_eq!(result.trim(), "ok");

    drop(root); // cleanup
}

#[test]
fn exec_should_forward_non_zero_exit_code() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    write_bin_failure_script(&workspace, "fail");

    pacquet_command(&workspace).with_args(["exec", "fail"]).assert().failure();

    drop(root); // cleanup
}

#[test]
fn exec_should_set_package_context_env_for_project() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "exec-fixture",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write package.json");
    write_bin_dump_env_script(&workspace, "dump-env");

    pacquet_command(&workspace).with_args(["exec", "dump-env"]).assert().success();

    let lines = fs::read_to_string(workspace.join("exec-env.txt")).expect("read exec env");
    assert_eq!(lines.lines().collect::<Vec<_>>(), vec!["exec-fixture", "exec"]);

    drop(root); // cleanup
}
