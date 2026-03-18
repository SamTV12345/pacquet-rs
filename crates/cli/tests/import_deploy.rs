use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::{fs, process::Command};

fn pacquet_command(workspace: &std::path::Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn import_should_generate_lockfile_from_existing_legacy_lockfile() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
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
        workspace.join("package-lock.json"),
        "{\"name\":\"workspace\",\"lockfileVersion\":3,\"packages\":{}}\n",
    )
    .expect("write package-lock.json");

    pacquet_command(&workspace).with_arg("import").assert().success();
    assert!(workspace.join("pnpm-lock.yaml").is_file());

    drop(root);
}

#[test]
fn import_should_prefer_versions_from_package_lock() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin": "*"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    fs::write(
        workspace.join("package-lock.json"),
        serde_json::json!({
            "name": "workspace",
            "lockfileVersion": 3,
            "packages": {
                "": {
                    "name": "workspace",
                    "version": "1.0.0"
                },
                "node_modules/@pnpm.e2e/hello-world-js-bin": {
                    "version": "0.0.0"
                }
            }
        })
        .to_string(),
    )
    .expect("write package-lock.json");

    pacquet_command(&workspace).with_arg("import").assert().success();

    let lockfile = fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read lockfile");
    assert!(lockfile.contains("@pnpm.e2e/hello-world-js-bin@0.0.0"));

    drop((root, mock_instance));
}

#[test]
fn deploy_should_fail_outside_workspace() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let target = root.path().join("deploy-target");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write package.json");

    let assert = pacquet_command(&workspace)
        .with_args(["deploy", target.to_string_lossy().as_ref()])
        .assert()
        .failure();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stderr.contains("only possible from inside a workspace"));

    drop(root);
}

#[test]
fn deploy_should_copy_filtered_workspace_project_from_root() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let target = root.path().join("deploy-target");
    let app_dir = workspace.join("packages").join("app");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
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
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(app_dir.join("index.js"), "console.log('ok');\n").expect("write app index.js");

    pacquet_command(&workspace)
        .with_args(["--filter=app", "deploy", target.to_string_lossy().as_ref()])
        .assert()
        .success();

    assert!(target.join("package.json").is_file());
    assert!(target.join("index.js").is_file());
    assert!(target.join("node_modules").is_dir());

    drop(root);
}

#[test]
fn deploy_should_fail_from_workspace_root_without_selected_project() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let target = root.path().join("deploy-target");
    let app_dir = workspace.join("packages").join("app");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
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
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app package.json");

    let assert = pacquet_command(&workspace)
        .with_args(["deploy", target.to_string_lossy().as_ref()])
        .assert()
        .failure();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stderr.contains("No workspace project was selected for deployment"));

    drop(root);
}

#[test]
fn deploy_should_materialize_workspace_dependency_from_shared_lockfile() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let target = root.path().join("deploy-target");
    let app_dir = workspace.join("packages").join("app");
    let lib_dir = workspace.join("packages").join("lib");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create lib dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
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
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write lib package.json");
    fs::write(lib_dir.join("index.js"), "module.exports = 'lib';\n").expect("write lib index.js");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/lib": "workspace:*"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");

    pacquet.with_args(["-C", workspace.to_str().unwrap(), "install", "-r"]).assert().success();
    pacquet_command(&workspace)
        .with_args(["--filter=app", "deploy", target.to_string_lossy().as_ref()])
        .assert()
        .success();

    let deployed_manifest =
        fs::read_to_string(target.join("package.json")).expect("read deployed package.json");
    assert!(deployed_manifest.contains("\"@repo/lib\": \"file:"));
    assert!(target.join("node_modules/@repo/lib/package.json").is_file());
    assert!(target.join("node_modules/@repo/lib/index.js").is_file());

    let deploy_lockfile =
        fs::read_to_string(target.join("pnpm-lock.yaml")).expect("read deploy pnpm-lock.yaml");
    assert!(deploy_lockfile.contains("@repo/lib@file:"));

    drop(root);
}
