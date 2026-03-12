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
fn unlink_should_remove_linked_dependency_from_manifest() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let linked_dir = workspace.join("../linked-lib");
    fs::create_dir_all(&linked_dir).expect("create linked dir");
    fs::write(
        linked_dir.join("package.json"),
        serde_json::json!({
            "name": "linked-lib",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write linked manifest");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write workspace manifest");

    pacquet_command(&workspace).with_args(["link", "../linked-lib"]).assert().success();
    pacquet_command(&workspace).with_args(["unlink", "linked-lib"]).assert().success();

    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(workspace.join("package.json")).expect("read workspace manifest"),
    )
    .expect("parse workspace manifest");
    assert!(manifest.get("dependencies").is_none());

    drop(root);
}

#[test]
fn unlink_should_print_nothing_to_unlink_when_no_link_is_present() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "dependencies": {
                "left-pad": "^1.3.0"
            }
        })
        .to_string(),
    )
    .expect("write workspace manifest");

    let assert = pacquet_command(&workspace).with_arg("unlink").assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Nothing to unlink"));

    drop(root);
}

#[test]
fn unlink_recursive_should_remove_workspace_override() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write workspace yaml");
    fs::create_dir_all(workspace.join("packages/app")).expect("create app dir");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write root manifest");
    fs::write(
        workspace.join("packages/app/package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    let linked_dir = workspace.join("../linked-lib");
    fs::create_dir_all(&linked_dir).expect("create linked dir");
    fs::write(
        linked_dir.join("package.json"),
        serde_json::json!({
            "name": "linked-lib",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write linked manifest");

    pacquet_command(&workspace.join("packages/app"))
        .with_args(["link", "../../../linked-lib"])
        .assert()
        .success();
    pacquet_command(&workspace).with_args(["unlink", "-r", "linked-lib"]).assert().success();

    let workspace_yaml =
        fs::read_to_string(workspace.join("pnpm-workspace.yaml")).expect("read workspace yaml");
    assert!(!workspace_yaml.contains("linked-lib: link:"));

    let app_manifest: Value = serde_json::from_str(
        &fs::read_to_string(workspace.join("packages/app/package.json"))
            .expect("read app manifest"),
    )
    .expect("parse app manifest");
    assert!(app_manifest.get("dependencies").is_none());

    drop(root);
}
