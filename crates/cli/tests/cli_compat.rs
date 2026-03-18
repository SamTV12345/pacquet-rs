use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::{fs, path::Path, process::Command};

#[cfg(unix)]
fn write_unix_executable(path: &Path, content: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, content).expect("write executable");
    let mut permissions = fs::metadata(path).expect("read metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable permissions");
}

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

fn normalize_reported_path(path: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        return path
            .strip_prefix("/private/var/")
            .map_or_else(|| path.to_string(), |suffix| format!("/var/{suffix}"));
    }

    #[cfg(not(target_os = "macos"))]
    {
        path.to_string()
    }
}

fn write_bin_success_script(workspace: &Path, name: &str) {
    let bin_dir = workspace.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).expect("create node_modules/.bin");

    #[cfg(windows)]
    {
        fs::write(bin_dir.join(format!("{name}.cmd")), "@echo off\r\necho ok> cli-result.txt\r\n")
            .expect("write .cmd file");
    }

    #[cfg(unix)]
    {
        write_unix_executable(&bin_dir.join(name), "#!/bin/sh\necho ok > cli-result.txt\n");
    }
}

fn write_bin_append_line_script(workspace: &Path, name: &str, line: &str) {
    let bin_dir = workspace.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).expect("create node_modules/.bin");

    #[cfg(windows)]
    {
        fs::write(
            bin_dir.join(format!("{name}.cmd")),
            format!("@echo off\r\necho {line}>> recursive-result.txt\r\n"),
        )
        .expect("write .cmd file");
    }

    #[cfg(unix)]
    {
        write_unix_executable(
            &bin_dir.join(name),
            &format!("#!/bin/sh\necho {line} >> recursive-result.txt\n"),
        );
    }
}

#[test]
fn install_alias_i_should_show_install_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["i", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Install packages"));

    drop(root);
}

#[test]
fn ci_alias_ic_should_show_ci_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["ic", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("frozen lockfile"));

    drop(root);
}

#[test]
fn completion_should_emit_shell_script() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert =
        pacquet_command(&workspace).with_args(["completion", "powershell"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("pacquet"));
    assert!(stdout.contains("install"));

    drop(root);
}

#[test]
fn create_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["create", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("starter"));

    drop(root);
}

#[test]
fn deploy_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["deploy", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Deploy"));

    drop(root);
}

#[test]
fn pack_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["pack", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("tarball"));

    drop(root);
}

#[test]
fn patch_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["patch", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("patching"));

    drop(root);
}

#[test]
fn patch_commit_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert =
        pacquet_command(&workspace).with_args(["patch-commit", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Generate a patch"));

    drop(root);
}

#[test]
fn patch_remove_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert =
        pacquet_command(&workspace).with_args(["patch-remove", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Remove existing patch"));

    drop(root);
}

#[test]
fn import_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["import", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Generate"));

    drop(root);
}

#[test]
fn approve_builds_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert =
        pacquet_command(&workspace).with_args(["approve-builds", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Approve dependencies"));

    drop(root);
}

#[test]
fn ignored_builds_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert =
        pacquet_command(&workspace).with_args(["ignored-builds", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("blocked build scripts"));

    drop(root);
}

#[test]
fn licenses_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["licenses", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("licenses"));

    drop(root);
}

#[test]
fn audit_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["audit", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("security issues"));

    drop(root);
}

#[test]
fn publish_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["publish", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Publishes"));

    drop(root);
}

#[test]
fn self_update_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert =
        pacquet_command(&workspace).with_args(["self-update", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Update pacquet"));

    drop(root);
}

#[test]
fn server_should_show_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["server", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("store server"));

    drop(root);
}

#[test]
fn setup_should_write_launcher_scripts_to_pnpm_home() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let pnpm_home = root.path().join("pnpm-home");

    pacquet_command(&workspace)
        .with_env("PNPM_HOME", &pnpm_home)
        .with_arg("setup")
        .assert()
        .success();

    #[cfg(windows)]
    {
        assert!(pnpm_home.join("pnpm.cmd").exists());
        assert!(pnpm_home.join("pnpx.cmd").exists());
    }

    #[cfg(not(windows))]
    {
        assert!(pnpm_home.join("pnpm").exists());
        assert!(pnpm_home.join("pnpx").exists());
    }

    drop(root);
}

#[test]
fn run_script_alias_should_execute_script() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

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

    pacquet_command(&workspace).with_args(["run-script", "hello"]).assert().success();

    let result = fs::read_to_string(workspace.join("cli-result.txt")).expect("read cli result");
    assert_eq!(result.trim(), "ok");

    drop(root);
}

#[test]
fn top_level_script_fallback_should_execute_matching_script() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

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

    pacquet_command(&workspace).with_arg("hello").assert().success();

    let result = fs::read_to_string(workspace.join("cli-result.txt")).expect("read cli result");
    assert_eq!(result.trim(), "ok");

    drop(root);
}

#[test]
fn top_level_t_alias_should_execute_test_script() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {
                "test": "hello-bin"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    write_bin_success_script(&workspace, "hello-bin");

    pacquet_command(&workspace).with_arg("t").assert().success();

    let result = fs::read_to_string(workspace.join("cli-result.txt")).expect("read cli result");
    assert_eq!(result.trim(), "ok");

    drop(root);
}

#[test]
fn unknown_builtin_without_matching_script_should_not_fallback_to_run() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

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

    pacquet_command(&workspace).with_arg("publish").assert().failure();

    drop(root);
}

#[test]
fn update_alias_up_should_show_update_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["up", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Update"));

    drop(root);
}

#[test]
fn rebuild_alias_rb_should_show_rebuild_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["rb", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Rebuild"));

    drop(root);
}

#[test]
fn exec_shell_mode_should_run_through_shell() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    pacquet_command(&workspace)
        .with_args(["exec", "--shell-mode", "echo shell-mode > shell-mode.txt"])
        .assert()
        .success();

    let result =
        fs::read_to_string(workspace.join("shell-mode.txt")).expect("read shell mode result");
    assert!(result.contains("shell-mode"));

    drop(root);
}

#[test]
fn root_should_print_local_node_modules_dir() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_arg("root").assert().success();
    let stdout =
        normalize_reported_path(String::from_utf8_lossy(&assert.get_output().stdout).trim());
    let expected = normalize_reported_path(&workspace.join("node_modules").display().to_string());
    assert_eq!(stdout, expected);

    drop(root);
}

#[test]
fn root_global_should_print_global_node_modules_dir() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let global_home = root.path().join("pnpm-home");
    fs::create_dir_all(&global_home).expect("create pnpm home");

    let assert = pacquet_command(&workspace)
        .with_env("PNPM_HOME", &global_home)
        .with_args(["root", "--global"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).trim().to_string();
    assert_eq!(stdout, global_home.join("global").join("node_modules").display().to_string());

    drop(root);
}

#[test]
fn help_run_should_show_run_help() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    let assert = pacquet_command(&workspace).with_args(["help", "run"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Usage: run"));

    drop(root);
}

#[test]
fn restart_should_run_stop_restart_and_start_scripts() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {
                "stop": "stop-bin",
                "restart": "restart-bin",
                "start": "start-bin"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    write_bin_append_line_script(&workspace, "stop-bin", "stop");
    write_bin_append_line_script(&workspace, "restart-bin", "restart");
    write_bin_append_line_script(&workspace, "start-bin", "start");

    pacquet_command(&workspace).with_arg("restart").assert().success();

    let result =
        fs::read_to_string(workspace.join("recursive-result.txt")).expect("read restart result");
    assert_eq!(result.replace("\r\n", "\n"), "stop\nrestart\nstart\n");

    drop(root);
}

#[test]
fn install_test_alias_should_install_then_run_test() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {
                "test": "test-bin"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    write_bin_success_script(&workspace, "test-bin");

    pacquet_command(&workspace).with_arg("it").assert().success();

    let result =
        fs::read_to_string(workspace.join("cli-result.txt")).expect("read install-test result");
    assert_eq!(result.trim(), "ok");

    drop(root);
}

#[test]
fn recursive_run_should_forward_to_run_recursive() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();
    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create lib dir");

    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "private": true
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "scripts": {
                "mark": "app-mark"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "lib",
            "version": "1.0.0",
            "scripts": {
                "mark": "lib-mark"
            }
        })
        .to_string(),
    )
    .expect("write lib package.json");

    write_bin_append_line_script(&workspace, "app-mark", "app");
    write_bin_append_line_script(&workspace, "lib-mark", "lib");

    pacquet_command(&workspace).with_args(["recursive", "run", "mark"]).assert().success();

    let app_result = fs::read_to_string(app_dir.join("recursive-result.txt"))
        .expect("read app recursive result");
    let lib_result = fs::read_to_string(lib_dir.join("recursive-result.txt"))
        .expect("read lib recursive result");
    assert!(app_result.contains("app"));
    assert!(lib_result.contains("lib"));

    drop(root);
}
