pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::{AddMockedRegistry, CommandTempCwd};
use std::{path::Path, process::Command};

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn cache_list_and_list_registries_should_show_cached_metadata() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    pacquet_command(&workspace)
        .with_args(["add", "@pnpm.e2e/hello-world-js-bin@1.0.0"])
        .assert()
        .success();

    let list = pacquet_command(&workspace).with_args(["cache", "list"]).assert().success();
    let list_stdout = String::from_utf8_lossy(&list.get_output().stdout).to_string();
    assert!(list_stdout.contains("@pnpm.e2e%2fhello-world-js-bin.json"));

    let registries =
        pacquet_command(&workspace).with_args(["cache", "list-registries"]).assert().success();
    let registries_stdout = String::from_utf8_lossy(&registries.get_output().stdout).to_string();
    let registry_namespace = mock_instance
        .url()
        .trim_end_matches('/')
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .replace(':', "+");
    assert!(registries_stdout.contains(&registry_namespace));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn cache_view_and_delete_should_inspect_and_remove_package_cache() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { cache_dir, mock_instance, .. } = npmrc_info;

    pacquet_command(&workspace)
        .with_args(["add", "@pnpm.e2e/hello-world-js-bin@1.0.0"])
        .assert()
        .success();

    let view = pacquet_command(&workspace)
        .with_args(["cache", "view", "@pnpm.e2e/hello-world-js-bin"])
        .assert()
        .success();
    let view_stdout = String::from_utf8_lossy(&view.get_output().stdout).to_string();
    let view_json: serde_json::Value =
        serde_json::from_str(&view_stdout).expect("parse cache view");
    assert!(view_json.as_object().is_some_and(|object| !object.is_empty()));

    pacquet_command(&workspace)
        .with_args(["cache", "delete", "*hello-world-js-bin"])
        .assert()
        .success();

    let cached_file = cache_dir
        .join("metadata-v1.3")
        .join(
            mock_instance
                .url()
                .trim_end_matches('/')
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .replace(':', "+"),
        )
        .join("@pnpm.e2e%2fhello-world-js-bin.json");
    assert!(!cached_file.exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn cache_view_should_require_exactly_one_package_name() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    pacquet_command(&workspace).with_args(["cache", "view"]).assert().failure();
    pacquet_command(&workspace).with_args(["cache", "view", "a", "b"]).assert().failure();

    drop(root); // cleanup
}
