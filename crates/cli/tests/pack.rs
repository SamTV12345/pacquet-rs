use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use flate2::read::GzDecoder;
use pacquet_testing_utils::bin::CommandTempCwd;
use serde_json::Value;
use std::{fs, path::Path, process::Command};
use tar::Archive;

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn pack_should_create_default_tarball() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "@scope/demo",
            "version": "1.2.3",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write package.json");
    fs::write(workspace.join("index.js"), "module.exports = 'demo';\n").expect("write index.js");

    pacquet_command(&workspace).with_arg("pack").assert().success();

    let tarball = workspace.join("scope-demo-1.2.3.tgz");
    assert!(tarball.is_file());

    let archive = fs::File::open(&tarball).expect("open tarball");
    let decoder = GzDecoder::new(archive);
    let mut archive = Archive::new(decoder);
    let mut entries = archive
        .entries()
        .expect("read tar entries")
        .map(|entry| {
            entry
                .expect("read tar entry")
                .path()
                .expect("read tar entry path")
                .display()
                .to_string()
                .replace('\\', "/")
        })
        .collect::<Vec<_>>();
    entries.sort();

    assert!(entries.contains(&"package/index.js".to_string()));
    assert!(entries.contains(&"package/package.json".to_string()));

    drop(root);
}

#[test]
fn pack_json_dry_run_should_not_write_tarball() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "demo",
            "version": "0.1.0"
        })
        .to_string(),
    )
    .expect("write package.json");
    fs::write(workspace.join("index.js"), "module.exports = 'demo';\n").expect("write index.js");

    let assert =
        pacquet_command(&workspace).with_args(["pack", "--json", "--dry-run"]).assert().success();
    let output: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("parse pack json");

    assert_eq!(output["name"], "demo");
    assert_eq!(output["version"], "0.1.0");
    assert_eq!(output["filename"], "demo-0.1.0.tgz");
    assert!(!workspace.join("demo-0.1.0.tgz").exists());

    drop(root);
}
