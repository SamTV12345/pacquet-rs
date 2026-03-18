use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use serde_json::Value;
use std::{fs, path::Path, process::Command, sync::Mutex};

static REGISTRY_TEST_LOCK: Mutex<()> = Mutex::new(());

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

fn dependency_spec(manifest_path: &Path, dependency_name: &str) -> String {
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(manifest_path).expect("read package.json after update"),
    )
    .expect("parse package.json after update");
    manifest["dependencies"][dependency_name].as_str().expect("dependency string").to_string()
}

#[test]
fn update_should_apply_explicit_package_spec() {
    let _guard = REGISTRY_TEST_LOCK.lock().expect("lock update registry tests");
    let CommandTempCwd { root, workspace, npmrc_info: _npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();

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
    pacquet_command(&workspace).with_args(["update", "is-positive@2.0.0"]).assert().success();

    let updated = dependency_spec(&workspace.join("package.json"), "is-positive");
    assert_eq!(updated, "2.0.0");

    assert!(workspace.join("node_modules/.pnpm/is-positive@2.0.0").exists());

    drop(root);
}

#[test]
fn update_latest_should_ignore_current_exact_version() {
    let _guard = REGISTRY_TEST_LOCK.lock().expect("lock update registry tests");
    let CommandTempCwd { root, workspace, npmrc_info: _npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();

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
    pacquet_command(&workspace).with_args(["update", "--latest", "is-positive"]).assert().success();

    let updated = dependency_spec(&workspace.join("package.json"), "is-positive");
    assert_ne!(updated, "1.0.0");

    let lockfile = fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read pnpm-lock");
    assert!(!lockfile.contains("is-positive: 1.0.0"));

    drop(root);
}
