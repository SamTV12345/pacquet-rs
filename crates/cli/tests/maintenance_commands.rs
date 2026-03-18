use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::{fs, process::Command};

fn pacquet_command(workspace: &std::path::Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn self_update_should_write_package_manager_field() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_args(["self-update", "9.9.9"]).assert().success();

    let package_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(workspace.join("package.json")).expect("read package.json"),
    )
    .expect("parse package.json");
    assert_eq!(package_json["packageManager"], "pacquet@9.9.9");

    drop(root);
}

#[test]
fn server_should_start_report_status_and_stop() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let pnpm_home = root.path().join("pnpm-home");

    pacquet_command(&workspace)
        .with_env("PNPM_HOME", &pnpm_home)
        .with_args(["server", "start", "--port", "4444"])
        .assert()
        .success();

    let status = pacquet_command(&workspace)
        .with_env("PNPM_HOME", &pnpm_home)
        .with_args(["server", "status"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&status.get_output().stdout);
    assert!(stdout.contains("Store server is running"));
    assert!(stdout.contains("4444"));

    pacquet_command(&workspace)
        .with_env("PNPM_HOME", &pnpm_home)
        .with_args(["server", "stop"])
        .assert()
        .success();

    drop(root);
}
