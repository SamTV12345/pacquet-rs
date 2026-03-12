pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::{fs, path::Path, process::Command};

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn bin_should_print_local_bin_directory() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let expected = workspace.join("node_modules/.bin");
    fs::create_dir_all(&expected).expect("create node_modules/.bin");

    let assert = pacquet_command(&workspace).with_arg("bin").assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).trim().to_string();
    assert_eq!(
        fs::canonicalize(stdout).expect("canonicalize command output"),
        fs::canonicalize(expected).expect("canonicalize expected path")
    );

    drop(root);
}

#[test]
fn bin_global_should_prefer_pnpm_home() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let pnpm_home = workspace.join("pnpm-home");

    let assert = pacquet_command(&workspace)
        .with_args(["bin", "-g"])
        .env("PNPM_HOME", &pnpm_home)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).trim().to_string();
    assert_eq!(stdout, pnpm_home.display().to_string());

    drop(root);
}
