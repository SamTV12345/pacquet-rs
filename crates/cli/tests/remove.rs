use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_testing_utils::bin::CommandTempCwd;
use std::process::Command;

fn pacquet_command(workspace: &std::path::Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn should_remove_dependency_from_manifest() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;

    pacquet_command(&workspace)
        .with_args(["add", "@pnpm.e2e/hello-world-js-bin"])
        .assert()
        .success();
    pacquet_command(&workspace)
        .with_args(["remove", "@pnpm.e2e/hello-world-js-bin"])
        .assert()
        .success();

    let manifest =
        PackageManifest::from_path(workspace.join("package.json")).expect("load manifest");
    let still_present = manifest
        .dependencies([
            DependencyGroup::Prod,
            DependencyGroup::Dev,
            DependencyGroup::Optional,
            DependencyGroup::Peer,
        ])
        .any(|(name, _)| name == "@pnpm.e2e/hello-world-js-bin");
    assert!(!still_present);

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_fail_when_dependency_is_missing() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;

    pacquet_command(&workspace)
        .with_args(["remove", "@pnpm.e2e/hello-world-js-bin"])
        .assert()
        .failure();
    assert!(workspace.join("package.json").exists());

    drop((root, mock_instance)); // cleanup
}
