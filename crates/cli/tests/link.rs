pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use serde_json::Value;
use std::{fs, path::Path, process::Command};

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn link_relative_path_should_add_link_dependency_and_install() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let linked_dir = workspace.join("../linked-bin");
    fs::create_dir_all(&linked_dir).expect("create linked dir");
    fs::write(
        linked_dir.join("package.json"),
        serde_json::json!({
            "name": "linked-bin",
            "version": "1.0.0",
            "bin": "bin.js"
        })
        .to_string(),
    )
    .expect("write linked manifest");
    fs::write(linked_dir.join("bin.js"), "#!/usr/bin/env node\nconsole.log('hi')\n")
        .expect("write linked bin");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write workspace manifest");

    pacquet_command(&workspace).with_args(["link", "../linked-bin"]).assert().success();

    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(workspace.join("package.json")).expect("read workspace manifest"),
    )
    .expect("parse workspace manifest");
    assert_eq!(manifest["dependencies"]["linked-bin"], "link:../linked-bin");
    assert!(workspace.join("node_modules/linked-bin/package.json").exists());
    assert!(bin_path(&workspace.join("node_modules/.bin"), "linked-bin").exists());
    drop(root);
}

#[test]
fn link_without_params_should_register_global_package_and_link_by_name() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let pnpm_home = workspace.join(".pnpm-home");
    let global_pkg = workspace.join("../global-pkg");
    fs::create_dir_all(&global_pkg).expect("create global package dir");
    fs::write(
        global_pkg.join("package.json"),
        serde_json::json!({
            "name": "global-pkg",
            "version": "1.0.0",
            "bin": "cli.js"
        })
        .to_string(),
    )
    .expect("write global package manifest");
    fs::write(global_pkg.join("cli.js"), "#!/usr/bin/env node\nconsole.log('global')\n")
        .expect("write global package bin");

    let mut link_global = pacquet_command(&global_pkg);
    link_global.env("PNPM_HOME", &pnpm_home);
    link_global.with_arg("link").assert().success();

    assert!(pnpm_home.join("global/node_modules/global-pkg/package.json").exists());
    assert!(bin_path(&pnpm_home, "global-pkg").exists());

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write workspace manifest");

    let mut link_from_global = pacquet_command(&workspace);
    link_from_global.env("PNPM_HOME", &pnpm_home);
    link_from_global.with_args(["link", "global-pkg"]).assert().success();

    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(workspace.join("package.json")).expect("read workspace manifest"),
    )
    .expect("parse workspace manifest");
    let expected_link_target = relative_path(
        &fs::canonicalize(pnpm_home.join("global/node_modules/global-pkg"))
            .expect("canonicalize global link target"),
        &fs::canonicalize(&workspace).expect("canonicalize workspace"),
    );
    assert_eq!(
        manifest["dependencies"]["global-pkg"],
        format!("link:{}", expected_link_target.to_string_lossy().replace('\\', "/"))
    );
    assert!(workspace.join("node_modules/global-pkg/package.json").exists());
    drop(root);
}

#[test]
fn link_in_workspace_should_write_workspace_override() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write workspace yaml");
    fs::create_dir_all(workspace.join("packages/app")).expect("create app dir");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write workspace root manifest");
    fs::write(
        workspace.join("packages/app/package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");

    let linked_dir = workspace.join("../linked-lib");
    fs::create_dir_all(&linked_dir).expect("create linked lib dir");
    fs::write(
        linked_dir.join("package.json"),
        serde_json::json!({
            "name": "linked-lib",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write linked lib manifest");

    pacquet_command(&workspace.join("packages/app"))
        .with_args(["link", "../../../linked-lib"])
        .assert()
        .success();

    let workspace_yaml =
        fs::read_to_string(workspace.join("pnpm-workspace.yaml")).expect("read workspace yaml");
    assert!(workspace_yaml.contains("overrides:"));
    assert!(workspace_yaml.contains("linked-lib: link:../linked-lib"));
    drop(root);
}

fn bin_path(bin_dir: &Path, name: &str) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        bin_dir.join(format!("{name}.cmd"))
    }
    #[cfg(not(windows))]
    {
        bin_dir.join(name)
    }
}

fn relative_path(path: &Path, base: &Path) -> std::path::PathBuf {
    let path = path.components().collect::<Vec<_>>();
    let base = base.components().collect::<Vec<_>>();
    let common = path.iter().zip(base.iter()).take_while(|(left, right)| left == right).count();
    let mut result = std::path::PathBuf::new();
    for _ in common..base.len() {
        result.push("..");
    }
    for component in path.iter().skip(common) {
        result.push(component.as_os_str());
    }
    result
}
