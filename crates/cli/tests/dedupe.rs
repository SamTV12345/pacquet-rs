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
fn dedupe_should_upgrade_lockfile_and_node_modules_when_manifest_range_allows_newer_version() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init().add_mocked_registry();
    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("src");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create src dir");

    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write lib package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/lib": "file:../src"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install"])
        .assert()
        .success();

    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "2.0.0"
            }
        })
        .to_string(),
    )
    .expect("update lib package.json");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "dedupe"])
        .assert()
        .success();

    assert!(app_dir.join("node_modules/.pnpm/is-positive@2.0.0").exists());
    let lockfile = fs::read_to_string(app_dir.join("pnpm-lock.yaml")).expect("read lockfile");
    assert!(lockfile.contains("is-positive: 2.0.0"));

    drop(root);
}

#[test]
fn dedupe_check_should_fail_without_mutating_workspace_lockfile() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init().add_mocked_registry();
    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("src");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create src dir");

    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write lib package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/lib": "file:../src"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install"])
        .assert()
        .success();

    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "2.0.0"
            }
        })
        .to_string(),
    )
    .expect("update lib package.json");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "dedupe", "--check"])
        .assert()
        .failure();

    let lockfile = fs::read_to_string(app_dir.join("pnpm-lock.yaml")).expect("read lockfile");
    assert!(lockfile.contains("is-positive: 1.0.0"));
    assert!(!lockfile.contains("is-positive: 2.0.0"));
    assert!(app_dir.join("node_modules/.pnpm/is-positive@1.0.0").exists());

    drop(root);
}

#[test]
fn dedupe_reporter_silent_should_suppress_output() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init().add_mocked_registry();
    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("src");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create src dir");

    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write lib package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/lib": "file:../src"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install"])
        .assert()
        .success();

    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "2.0.0"
            }
        })
        .to_string(),
    )
    .expect("update lib package.json");

    let assert = pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "dedupe", "--reporter", "silent"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stdout.trim().is_empty());
    assert!(stderr.trim().is_empty());

    drop(root);
}
