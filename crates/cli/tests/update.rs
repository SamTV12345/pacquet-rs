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

#[test]
fn update_should_roll_workspace_ranges_in_recursive_workspace() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let project_1_dir = workspace.join("project-1");
    let project_2_dir = workspace.join("project-2");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - '*'\n")
        .expect("write pnpm-workspace.yaml");

    let dependency_map = serde_json::json!({
        "dep1": "workspace:1.0.0",
        "dep2": "workspace:~1.0.0",
        "dep3": "workspace:^1.0.0",
        "dep4": "workspace:1",
        "dep5": "workspace:1.0",
        "dep6": "workspace:*",
        "dep7": "workspace:^",
        "dep8": "workspace:~"
    });
    for project_dir in [&project_1_dir, &project_2_dir] {
        fs::write(
            project_dir.join("package.json"),
            serde_json::json!({
                "name": project_dir.file_name().and_then(|name| name.to_str()).unwrap(),
                "version": "1.0.0",
                "dependencies": dependency_map.clone()
            })
            .to_string(),
        )
        .expect("write project manifest");
    }

    for dep in ["dep1", "dep2", "dep3", "dep4", "dep5", "dep6", "dep7", "dep8"] {
        let dep_dir = workspace.join(dep);
        fs::create_dir_all(&dep_dir).expect("create workspace dep dir");
        fs::write(
            dep_dir.join("package.json"),
            serde_json::json!({
                "name": dep,
                "version": "2.0.0"
            })
            .to_string(),
        )
        .expect("write workspace dep manifest");
    }

    pacquet_command(&workspace)
        .with_args(["update", "--filter", "project-1", "--filter", "project-2"])
        .assert()
        .success();

    let expected = serde_json::json!({
        "dep1": "workspace:*",
        "dep2": "workspace:~",
        "dep3": "workspace:^",
        "dep4": "workspace:^",
        "dep5": "workspace:~",
        "dep6": "workspace:*",
        "dep7": "workspace:^",
        "dep8": "workspace:~"
    });

    for manifest_path in [project_1_dir.join("package.json"), project_2_dir.join("package.json")] {
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(&manifest_path).expect("read manifest"))
                .expect("parse manifest");
        assert_eq!(manifest["dependencies"], expected);
    }

    drop(root);
}

#[test]
fn update_workspace_should_fail_when_latest_is_set() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let project_1_dir = workspace.join("project-1");
    let project_2_dir = workspace.join("project-2");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - '*'\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "project-2": "^2.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "2.0.0"
        })
        .to_string(),
    )
    .expect("write project-2 manifest");

    let assert = pacquet_command(&project_1_dir)
        .with_args(["update", "--workspace", "--latest", "project-2"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(stderr.contains("Cannot use --latest with --workspace simultaneously"));

    drop(root);
}

#[test]
fn update_workspace_should_convert_direct_workspace_dependencies_from_manifest() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let project_1_dir = workspace.join("project-1");
    let foo_dir = workspace.join("foo");
    let bar_dir = workspace.join("bar");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&foo_dir).expect("create foo dir");
    fs::create_dir_all(&bar_dir).expect("create bar dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - '*'\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "foo": "1.0.0",
                "alpha": "1.0.0"
            },
            "devDependencies": {
                "bar": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        foo_dir.join("package.json"),
        serde_json::json!({
            "name": "foo",
            "version": "100.0.0"
        })
        .to_string(),
    )
    .expect("write foo manifest");
    fs::write(
        bar_dir.join("package.json"),
        serde_json::json!({
            "name": "bar",
            "version": "100.0.0"
        })
        .to_string(),
    )
    .expect("write bar manifest");

    pacquet_command(&project_1_dir).with_args(["update", "--workspace"]).assert().success();

    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(project_1_dir.join("package.json")).expect("read project-1 manifest"),
    )
    .expect("parse project-1 manifest");
    assert_eq!(manifest["dependencies"]["foo"], "workspace:*");
    assert_eq!(manifest["dependencies"]["alpha"], "1.0.0");
    assert_eq!(manifest["devDependencies"]["bar"], "workspace:*");

    drop(root);
}

#[test]
fn update_workspace_should_only_include_selected_dependency_groups() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let project_1_dir = workspace.join("project-1");
    let foo_dir = workspace.join("foo");
    let bar_dir = workspace.join("bar");
    let qar_dir = workspace.join("qar");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&foo_dir).expect("create foo dir");
    fs::create_dir_all(&bar_dir).expect("create bar dir");
    fs::create_dir_all(&qar_dir).expect("create qar dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - '*'\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "foo": "1.0.0"
            },
            "devDependencies": {
                "bar": "1.0.0"
            },
            "optionalDependencies": {
                "qar": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    for (dir, name) in [(&foo_dir, "foo"), (&bar_dir, "bar"), (&qar_dir, "qar")] {
        fs::write(
            dir.join("package.json"),
            serde_json::json!({
                "name": name,
                "version": "100.0.0"
            })
            .to_string(),
        )
        .expect("write workspace dep manifest");
    }

    pacquet_command(&project_1_dir)
        .with_args(["update", "--workspace", "--prod"])
        .assert()
        .success();

    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(project_1_dir.join("package.json")).expect("read project-1 manifest"),
    )
    .expect("parse project-1 manifest");
    assert_eq!(manifest["dependencies"]["foo"], "workspace:*");
    assert_eq!(manifest["devDependencies"]["bar"], "1.0.0");
    assert_eq!(manifest["optionalDependencies"]["qar"], "workspace:*");

    drop(root);
}

#[test]
fn update_workspace_should_normalize_explicit_workspace_specs() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let project_1_dir = workspace.join("project-1");
    let foo_dir = workspace.join("foo");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&foo_dir).expect("create foo dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - '*'\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "foo": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        foo_dir.join("package.json"),
        serde_json::json!({
            "name": "foo",
            "version": "100.0.0"
        })
        .to_string(),
    )
    .expect("write foo manifest");

    pacquet_command(&project_1_dir)
        .with_args(["update", "--workspace", "foo@100"])
        .assert()
        .success();

    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(project_1_dir.join("package.json")).expect("read project-1 manifest"),
    )
    .expect("parse project-1 manifest");
    assert_eq!(manifest["dependencies"]["foo"], "workspace:100");

    drop(root);
}

#[test]
fn update_workspace_should_preserve_explicit_workspace_protocol_specs() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let project_1_dir = workspace.join("project-1");
    let foo_dir = workspace.join("foo");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&foo_dir).expect("create foo dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - '*'\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "foo": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        foo_dir.join("package.json"),
        serde_json::json!({
            "name": "foo",
            "version": "100.0.0"
        })
        .to_string(),
    )
    .expect("write foo manifest");

    pacquet_command(&project_1_dir)
        .with_args(["update", "--workspace", "foo@workspace:100"])
        .assert()
        .success();

    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(project_1_dir.join("package.json")).expect("read project-1 manifest"),
    )
    .expect("parse project-1 manifest");
    assert_eq!(manifest["dependencies"]["foo"], "workspace:100");

    drop(root);
}

#[test]
fn update_workspace_should_fail_for_missing_workspace_package() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let project_1_dir = workspace.join("project-1");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - '*'\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write project-1 manifest");

    let assert = pacquet_command(&project_1_dir)
        .with_args(["update", "--workspace", "express"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(stderr.contains("\"express\" not found in the workspace"));

    drop(root);
}
