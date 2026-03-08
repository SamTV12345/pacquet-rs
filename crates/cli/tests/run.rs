pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::{fs, path::Path};

#[cfg(unix)]
fn write_unix_executable(path: &Path, content: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, content).expect("write executable");
    let mut permissions = fs::metadata(path).expect("read metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable permissions");
}

fn write_bin_success_script(workspace: &Path, name: &str) {
    let bin_dir = workspace.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).expect("create node_modules/.bin");

    #[cfg(windows)]
    {
        fs::write(bin_dir.join(format!("{name}.cmd")), "@echo off\r\necho ok> run-result.txt\r\n")
            .expect("write .cmd file");
    }

    #[cfg(unix)]
    {
        write_unix_executable(&bin_dir.join(name), "#!/bin/sh\necho ok > run-result.txt\n");
    }
}

fn write_bin_failure_script(workspace: &Path, name: &str) {
    let bin_dir = workspace.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).expect("create node_modules/.bin");

    #[cfg(windows)]
    {
        fs::write(bin_dir.join(format!("{name}.cmd")), "@echo off\r\nexit /b 7\r\n")
            .expect("write .cmd file");
    }

    #[cfg(unix)]
    {
        write_unix_executable(&bin_dir.join(name), "#!/bin/sh\nexit 7\n");
    }
}

fn write_bin_append_line_script(workspace: &Path, name: &str, line: &str) {
    let bin_dir = workspace.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).expect("create node_modules/.bin");

    #[cfg(windows)]
    {
        fs::write(
            bin_dir.join(format!("{name}.cmd")),
            format!("@echo off\r\necho {line}>> run-order.txt\r\n"),
        )
        .expect("write .cmd file");
    }

    #[cfg(unix)]
    {
        write_unix_executable(
            &bin_dir.join(name),
            &format!("#!/bin/sh\necho {line} >> run-order.txt\n"),
        );
    }
}

fn write_bin_dump_foo_script(workspace: &Path, name: &str) {
    let bin_dir = workspace.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).expect("create node_modules/.bin");

    #[cfg(windows)]
    {
        fs::write(
            bin_dir.join(format!("{name}.cmd")),
            "@echo off\r\necho %FOO%> env-result.txt\r\n",
        )
        .expect("write .cmd file");
    }

    #[cfg(unix)]
    {
        write_unix_executable(&bin_dir.join(name), "#!/bin/sh\necho \"$FOO\" > env-result.txt\n");
    }
}

#[test]
fn run_should_execute_script_from_node_modules_bin() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {
                "hello": "hello-bin"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    write_bin_success_script(&workspace, "hello-bin");

    pacquet.with_args(["run", "hello"]).assert().success();

    assert_eq!(
        fs::read_to_string(workspace.join("run-result.txt")).expect("read run-result.txt").trim(),
        "ok"
    );
    drop(root);
}

#[test]
fn run_if_present_should_succeed_when_script_is_missing() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {}
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet.with_args(["run", "missing", "--if-present"]).assert().success();
    drop(root);
}

#[test]
fn run_should_fail_when_script_returns_nonzero_exit_code() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {
                "bad": "bad-bin"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    write_bin_failure_script(&workspace, "bad-bin");

    pacquet.with_args(["run", "bad"]).assert().failure();
    drop(root);
}

#[test]
fn run_should_execute_pre_and_post_scripts_when_enabled() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {
                "prehello": "pre-bin",
                "hello": "main-bin",
                "posthello": "post-bin"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    fs::write(workspace.join(".npmrc"), "enable-pre-post-scripts=true\n").expect("write .npmrc");
    write_bin_append_line_script(&workspace, "pre-bin", "pre");
    write_bin_append_line_script(&workspace, "main-bin", "main");
    write_bin_append_line_script(&workspace, "post-bin", "post");

    pacquet.with_args(["run", "hello"]).assert().success();

    let lines = fs::read_to_string(workspace.join("run-order.txt")).expect("read run-order.txt");
    let lines = lines.lines().collect::<Vec<_>>();
    assert_eq!(lines, vec!["pre", "main", "post"]);
    drop(root);
}

#[test]
fn run_shell_emulator_should_support_env_prefix_and_and_chain() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {
                "hello": "FOO=bar foo-dump && chain-bin"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    fs::write(workspace.join(".npmrc"), "shell-emulator=true\n").expect("write .npmrc");
    write_bin_dump_foo_script(&workspace, "foo-dump");
    write_bin_append_line_script(&workspace, "chain-bin", "chain");

    pacquet.with_args(["run", "hello"]).assert().success();

    assert_eq!(
        fs::read_to_string(workspace.join("env-result.txt")).expect("read env-result.txt").trim(),
        "bar"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("run-order.txt")).expect("read run-order.txt").trim(),
        "chain"
    );
    drop(root);
}
