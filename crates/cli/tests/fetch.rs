pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::{
    bin::{AddMockedRegistry, CommandTempCwd},
    fs::get_all_files,
};
use std::{fs, path::Path, process::Command};

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn fetch_should_warm_store_from_lockfile_without_modifying_workspace() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_args(["install", "--lockfile-only"]).assert().success();
    assert!(!workspace.join("node_modules").exists());
    fs::remove_file(workspace.join("package.json")).expect("remove package.json");

    pacquet_command(&workspace).with_args(["fetch"]).assert().success();

    assert!(!workspace.join("node_modules").exists());
    assert!(get_all_files(&store_dir).into_iter().any(|path| path.contains("index")
        || path.contains("/files/")
        || path.contains("\\files\\")));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn fetch_should_fail_without_lockfile() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    pacquet_command(&workspace).with_args(["fetch"]).assert().failure();

    drop(root); // cleanup
}

#[test]
fn fetch_reporter_silent_should_suppress_output() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_args(["install", "--lockfile-only"]).assert().success();
    let assert =
        pacquet_command(&workspace).with_args(["fetch", "--reporter", "silent"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stdout.trim().is_empty());
    assert!(stderr.trim().is_empty());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn fetch_reporter_append_only_should_write_static_progress_lines() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_args(["install", "--lockfile-only"]).assert().success();
    let assert = pacquet_command(&workspace)
        .with_args(["fetch", "--reporter", "append-only"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stderr.contains("pacquet starting [frozen]"));
    assert!(stderr.contains("pacquet done [frozen]"));
    assert!(!stderr.contains("\u{1b}["));

    drop((root, mock_instance)); // cleanup
}
