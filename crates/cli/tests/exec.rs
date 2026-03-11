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

fn write_bin_append_package_name_script(workspace: &Path, name: &str) {
    let bin_dir = workspace.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).expect("create node_modules/.bin");

    #[cfg(windows)]
    {
        fs::write(
            bin_dir.join(format!("{name}.cmd")),
            format!("@echo off\r\necho %PNPM_PACKAGE_NAME%\r\n"),
        )
        .expect("write .cmd file");
    }

    #[cfg(unix)]
    {
        write_unix_executable(&bin_dir.join(name), "#!/bin/sh\necho \"$PNPM_PACKAGE_NAME\"\n");
    }
}

fn setup_exec_workspace_fixture(workspace: &Path) {
    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create lib dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace-root",
            "private": true
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "lib": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "lib",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write lib package.json");
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

#[test]
fn exec_recursive_should_run_in_all_workspace_projects() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    setup_exec_workspace_fixture(&workspace);
    write_bin_success_script(&workspace, "mark");

    pacquet_command(&workspace).with_args(["exec", "--recursive", "mark"]).assert().success();

    assert!(workspace.join("packages/app/exec-result.txt").exists());
    assert!(workspace.join("packages/lib/exec-result.txt").exists());
    assert!(workspace.join("exec-result.txt").exists());

    drop(root); // cleanup
}

#[test]
fn exec_recursive_report_summary_should_write_summary_file() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    setup_exec_workspace_fixture(&workspace);
    write_bin_success_script(&workspace, "mark");

    pacquet_command(&workspace)
        .with_args(["exec", "--recursive", "--report-summary", "mark"])
        .assert()
        .success();

    let summary =
        fs::read_to_string(workspace.join("pnpm-exec-summary.json")).expect("read summary");
    let summary: serde_json::Value = serde_json::from_str(&summary).expect("parse summary");
    assert_eq!(
        summary
            .get("executionStatus")
            .and_then(|status| status.get("packages/app"))
            .and_then(|entry| entry.get("status"))
            .and_then(|status| status.as_str()),
        Some("passed")
    );

    drop(root); // cleanup
}

#[test]
fn exec_recursive_no_reporter_hide_prefix_should_prefix_output() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    setup_exec_workspace_fixture(&workspace);
    write_bin_append_package_name_script(&workspace, "say-name");

    let assert = pacquet_command(&workspace)
        .with_args(["exec", "--recursive", "--parallel", "--no-reporter-hide-prefix", "say-name"])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(stdout.contains("app exec: app"));
    assert!(stdout.contains("lib exec: lib"));

    drop(root); // cleanup
}
