use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::{fs, process::Command};

fn pacquet_command(workspace: &std::path::Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn should_fail_without_lockfile() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
            },
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_arg("ci").assert().failure();

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_install_with_existing_lockfile() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
            },
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_arg("install").assert().success();
    pacquet_command(&workspace).with_arg("ci").assert().success();

    drop((root, mock_instance)); // cleanup
}
