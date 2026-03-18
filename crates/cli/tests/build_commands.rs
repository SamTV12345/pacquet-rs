use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::{fs, process::Command};

fn pacquet_command(workspace: &std::path::Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn ignored_builds_should_print_automatic_and_explicit_entries() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    fs::create_dir_all(workspace.join("node_modules")).expect("create node_modules");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "pnpm": {
                "ignoredBuiltDependencies": ["sharp"]
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    fs::write(
        workspace.join("node_modules/.modules.yaml"),
        "ignoredBuilds:\n  - /esbuild@1.0.0\n  - /sharp@2.0.0\npendingBuilds: []\n",
    )
    .expect("write .modules.yaml");

    let assert = pacquet_command(&workspace).with_arg("ignored-builds").assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Automatically ignored builds during installation:"));
    assert!(stdout.contains("esbuild"));
    assert!(!stdout.contains("sharp\nhint"));
    assert!(stdout.contains("Explicitly ignored package builds"));
    assert!(stdout.contains("sharp"));

    drop(root);
}

#[test]
fn approve_builds_should_promote_pending_packages_into_only_built_dependencies() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    fs::create_dir_all(workspace.join("node_modules/esbuild")).expect("create esbuild dir");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "pnpm": {
                "ignoredBuiltDependencies": ["esbuild"]
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    fs::write(
        workspace.join("node_modules/.modules.yaml"),
        "pendingBuilds:\n  - esbuild\nignoredBuilds:\n  - /esbuild@1.0.0\n",
    )
    .expect("write .modules.yaml");
    fs::write(
        workspace.join("node_modules/esbuild/package.json"),
        serde_json::json!({
            "name": "esbuild",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write installed package manifest");

    pacquet_command(&workspace).with_arg("approve-builds").assert().success();

    let package_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(workspace.join("package.json")).expect("read package.json"),
    )
    .expect("parse package.json");
    let pnpm =
        package_json.get("pnpm").and_then(serde_json::Value::as_object).expect("pnpm object");
    assert_eq!(
        pnpm.get("onlyBuiltDependencies")
            .and_then(serde_json::Value::as_array)
            .expect("onlyBuiltDependencies")
            .len(),
        1
    );
    assert!(pnpm.get("ignoredBuiltDependencies").is_none());

    let modules_yaml = fs::read_to_string(workspace.join("node_modules/.modules.yaml"))
        .expect("read .modules.yaml");
    assert!(modules_yaml.contains("pendingBuilds: []"));
    assert!(modules_yaml.contains("ignoredBuilds: []"));

    drop(root);
}
