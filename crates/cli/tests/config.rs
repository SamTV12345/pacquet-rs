pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use serde_json::Value;
use std::{fs, path::Path, process::Command};

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn config_list_should_print_sorted_project_settings() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    fs::write(workspace.join(".npmrc"), "store-dir=~/store\nfetch-retries=2\n")
        .expect("write .npmrc");

    let assert = pacquet_command(&workspace).with_args(["config", "list"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert_eq!(
        stdout,
        "fetch-retries=2\nregistry=https://registry.npmjs.org/\nstore-dir=~/store\n"
    );

    drop(root);
}

#[test]
fn config_list_json_should_print_sorted_settings_as_json() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    fs::write(workspace.join(".npmrc"), "store-dir=~/store\nfetch-retries=2\n")
        .expect("write .npmrc");

    let assert =
        pacquet_command(&workspace).with_args(["config", "list", "--json"]).assert().success();
    let output: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("parse json output");
    assert_eq!(output["fetch-retries"], "2");
    assert_eq!(output["store-dir"], "~/store");

    drop(root);
}

#[test]
fn config_get_should_support_camel_case_keys() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    fs::write(workspace.join(".npmrc"), "store-dir=~/store\n").expect("write .npmrc");

    let assert =
        pacquet_command(&workspace).with_args(["config", "get", "storeDir"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).trim().to_string();
    assert_eq!(stdout, "~/store");

    drop(root);
}

#[test]
fn config_set_and_delete_should_update_project_npmrc() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    pacquet_command(&workspace)
        .with_args(["config", "set", "--location", "project", "fetchRetries", "1"])
        .assert()
        .success();
    let written = fs::read_to_string(workspace.join(".npmrc")).expect("read .npmrc");
    assert!(written.contains("fetch-retries=1"));

    pacquet_command(&workspace)
        .with_args(["config", "delete", "--location", "project", "fetchRetries"])
        .assert()
        .success();
    assert!(!workspace.join(".npmrc").exists());

    drop(root);
}

#[test]
fn top_level_get_and_set_delegate_to_config() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    pacquet_command(&workspace)
        .with_args(["set", "--location", "project", "storeDir=~/store"])
        .assert()
        .success();
    let assert = pacquet_command(&workspace).with_args(["get", "store-dir"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).trim().to_string();
    assert_eq!(stdout, "~/store");

    drop(root);
}
