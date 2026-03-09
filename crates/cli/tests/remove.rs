use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_testing_utils::bin::CommandTempCwd;
use std::fs;
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

#[test]
fn workspace_remove_filter_should_target_selected_project_only() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create lib dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace-root",
            "version": "1.0.0",
            "dependencies": { "@pnpm.e2e/hello-world-js-bin": "1.0.0" }
        })
        .to_string(),
    )
    .expect("write root manifest");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/app",
            "version": "1.0.0",
            "dependencies": { "@pnpm.e2e/hello-world-js-bin": "1.0.0" }
        })
        .to_string(),
    )
    .expect("write app manifest");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "dependencies": { "@pnpm.e2e/hello-world-js-bin": "1.0.0" }
        })
        .to_string(),
    )
    .expect("write lib manifest");

    pacquet_command(&workspace).with_args(["install"]).assert().success();
    pacquet_command(&workspace)
        .with_args(["remove", "@pnpm.e2e/hello-world-js-bin", "--filter", "@repo/app"])
        .assert()
        .success();

    let root_manifest = PackageManifest::from_path(workspace.join("package.json")).unwrap();
    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    let lib_manifest = PackageManifest::from_path(lib_dir.join("package.json")).unwrap();

    assert!(
        root_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, _)| name == "@pnpm.e2e/hello-world-js-bin")
    );
    assert!(
        !app_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, _)| name == "@pnpm.e2e/hello-world-js-bin")
    );
    assert!(
        lib_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, _)| name == "@pnpm.e2e/hello-world-js-bin")
    );

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_remove_recursive_from_subproject_should_target_all_projects_including_root() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create lib dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace-root",
            "version": "1.0.0",
            "dependencies": { "@pnpm.e2e/hello-world-js-bin": "1.0.0" }
        })
        .to_string(),
    )
    .expect("write root manifest");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/app",
            "version": "1.0.0",
            "dependencies": { "@pnpm.e2e/hello-world-js-bin": "1.0.0" }
        })
        .to_string(),
    )
    .expect("write app manifest");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "dependencies": { "@pnpm.e2e/hello-world-js-bin": "1.0.0" }
        })
        .to_string(),
    )
    .expect("write lib manifest");

    pacquet
        .with_args([
            "-C",
            app_dir.to_str().unwrap(),
            "remove",
            "@pnpm.e2e/hello-world-js-bin",
            "--recursive",
        ])
        .assert()
        .success();

    let root_manifest = PackageManifest::from_path(workspace.join("package.json")).unwrap();
    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    let lib_manifest = PackageManifest::from_path(lib_dir.join("package.json")).unwrap();

    assert!(
        !root_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, _)| name == "@pnpm.e2e/hello-world-js-bin")
    );
    assert!(
        !app_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, _)| name == "@pnpm.e2e/hello-world-js-bin")
    );
    assert!(
        !lib_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, _)| name == "@pnpm.e2e/hello-world-js-bin")
    );

    drop((root, mock_instance)); // cleanup
}
