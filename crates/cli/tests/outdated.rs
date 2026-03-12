pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::{AddMockedRegistry, CommandTempCwd};
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::Command,
    sync::{Mutex, OnceLock},
};

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

fn registry_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn outdated_should_report_newer_registry_version() {
    let _guard = registry_test_lock();
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_arg("install").assert().success();

    let assert = pacquet_command(&workspace).with_arg("outdated").assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(stdout.contains("Package"));
    assert!(stdout.contains("Current"));
    assert!(stdout.contains("Latest"));
    assert!(stdout.contains("is-positive"));
    assert!(stdout.contains("1.0.0"));

    drop((root, mock_instance));
}

#[test]
fn outdated_json_should_emit_structured_entries() {
    let _guard = registry_test_lock();
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_arg("install").assert().success();

    let assert = pacquet_command(&workspace).with_args(["outdated", "--json"]).assert().success();
    let output: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("parse outdated json");
    let first = &output["is-positive"];
    assert_eq!(first["current"], "1.0.0");
    assert_ne!(first["latest"], "1.0.0");
    assert_eq!(first["wanted"], "1.0.0");
    assert_eq!(first["dependencyType"], "dependencies");
    assert_eq!(first["isDeprecated"], false);

    drop((root, mock_instance));
}

#[test]
fn outdated_list_long_should_include_details() {
    let _guard = registry_test_lock();
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_arg("install").assert().success();

    let assert = pacquet_command(&workspace)
        .with_args(["outdated", "--format", "list", "--long"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(stdout.contains("is-positive"));
    assert!(stdout.contains("=>"));
    assert!(stdout.lines().count() >= 3);

    drop((root, mock_instance));
}

#[test]
fn outdated_recursive_json_should_include_dependents() {
    let _guard = registry_test_lock();
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::create_dir_all(workspace.join("packages/app")).expect("create app dir");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        workspace.join("packages/app/package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");

    pacquet_command(&workspace).with_args(["install", "-r"]).assert().success();

    let assert =
        pacquet_command(&workspace).with_args(["outdated", "-r", "--json"]).assert().success();
    let output: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("parse outdated json");
    let dependent_packages =
        output["is-positive"]["dependentPackages"].as_array().expect("dependentPackages array");
    assert_eq!(dependent_packages.len(), 1);
    assert_eq!(dependent_packages[0]["name"], "app");
    assert!(
        dependent_packages[0]["location"]
            .as_str()
            .expect("dependent package location")
            .replace('\\', "/")
            .ends_with("/packages/app")
    );

    drop((root, mock_instance));
}
