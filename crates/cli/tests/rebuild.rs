use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::{fs, path::Path, process::Command};

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

fn write_file_script_command(file_name: &str, content: &str) -> String {
    #[cfg(windows)]
    {
        format!("cmd /c echo {content}>{file_name}")
    }

    #[cfg(not(windows))]
    {
        format!("sh -c 'echo {content} > {file_name}'")
    }
}

#[test]
fn rebuild_should_rerun_project_install_lifecycle() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {
                "install": write_file_script_command("rebuild.txt", "project")
            }
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_arg("rebuild").assert().success();

    let rebuilt = fs::read_to_string(workspace.join("rebuild.txt")).expect("read rebuild output");
    assert_eq!(rebuilt.trim(), "project");

    drop(root);
}

#[test]
fn rebuild_selected_dependency_should_rerun_installed_package_lifecycle() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let app_dir = workspace.join("app");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(app_dir.join("node_modules/dep")).expect("create installed dep dir");

    fs::write(
        app_dir.join("node_modules/dep/package.json"),
        serde_json::json!({
            "name": "dep",
            "version": "1.0.0",
            "scripts": {
                "install": write_file_script_command("dep-rebuilt.txt", "dependency")
            }
        })
        .to_string(),
    )
    .expect("write installed dep package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "dep": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");

    let installed_marker = app_dir.join("node_modules/dep/dep-rebuilt.txt");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "rebuild", "dep"])
        .assert()
        .success();

    let rebuilt = fs::read_to_string(installed_marker).expect("read dependency rebuild output");
    assert_eq!(rebuilt.trim(), "dependency");

    drop(root);
}
