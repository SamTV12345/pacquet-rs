use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::{fs, process::Command};

fn pacquet_command(workspace: &std::path::Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn patch_commit_and_remove_should_manage_patch_files_and_manifest_entries() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let edit_dir = workspace.join("edit-foo");

    fs::create_dir_all(workspace.join("node_modules/foo")).expect("create installed package dir");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write package.json");
    fs::write(
        workspace.join("node_modules/foo/package.json"),
        serde_json::json!({
            "name": "foo",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write installed package.json");
    fs::write(workspace.join("node_modules/foo/index.js"), "module.exports = 'before';\n")
        .expect("write installed file");

    pacquet_command(&workspace)
        .with_args(["patch", "foo", "--edit-dir", edit_dir.to_string_lossy().as_ref()])
        .assert()
        .success();

    fs::write(edit_dir.join("index.js"), "module.exports = 'after';\n")
        .expect("modify patched file");

    pacquet_command(&workspace)
        .with_args(["patch-commit", edit_dir.to_string_lossy().as_ref()])
        .assert()
        .success();

    let patch_file = workspace.join("patches/foo.patch");
    assert!(patch_file.is_file());
    let patch_content = fs::read_to_string(&patch_file).expect("read patch file");
    assert!(patch_content.contains("after"));

    let package_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(workspace.join("package.json")).expect("read package.json"),
    )
    .expect("parse package.json");
    assert_eq!(package_json["pnpm"]["patchedDependencies"]["foo"], "patches/foo.patch");

    pacquet_command(&workspace).with_args(["patch-remove", "foo"]).assert().success();

    let package_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(workspace.join("package.json")).expect("read package.json"),
    )
    .expect("parse package.json");
    assert!(package_json.get("pnpm").is_none());
    assert!(!patch_file.exists());

    drop(root);
}
