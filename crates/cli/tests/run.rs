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

fn write_bin_echo_script(workspace: &Path, name: &str, line: &str) {
    let bin_dir = workspace.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).expect("create node_modules/.bin");

    #[cfg(windows)]
    {
        fs::write(bin_dir.join(format!("{name}.cmd")), format!("@echo off\r\necho {line}\r\n"))
            .expect("write .cmd file");
    }

    #[cfg(unix)]
    {
        write_unix_executable(&bin_dir.join(name), &format!("#!/bin/sh\necho {line}\n"));
    }
}

fn setup_workspace_filter_fixture(workspace: &Path, root_script: &str, app_uses_dev_dep: bool) {
    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    let util_dir = workspace.join("packages/util");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create lib dir");
    fs::create_dir_all(&util_dir).expect("create util dir");

    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "private": true,
            "scripts": {
                "mark": root_script
            }
        })
        .to_string(),
    )
    .expect("write root package.json");

    let mut app = serde_json::json!({
        "name": "app",
        "version": "1.0.0",
        "scripts": {
            "mark": "app-mark"
        },
        "dependencies": {}
    });
    if app_uses_dev_dep {
        app["devDependencies"] = serde_json::json!({ "lib": "1.0.0" });
    } else {
        app["dependencies"] = serde_json::json!({ "lib": "1.0.0" });
    }
    fs::write(app_dir.join("package.json"), app.to_string()).expect("write app package.json");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "lib",
            "version": "1.0.0",
            "scripts": {
                "mark": "lib-mark"
            },
            "dependencies": {
                "util": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write lib package.json");
    fs::write(
        util_dir.join("package.json"),
        serde_json::json!({
            "name": "util",
            "version": "1.0.0",
            "scripts": {
                "mark": "util-mark"
            }
        })
        .to_string(),
    )
    .expect("write util package.json");

    write_bin_append_line_script(workspace, "app-mark", "app");
    write_bin_append_line_script(workspace, "lib-mark", "lib");
    write_bin_append_line_script(workspace, "util-mark", "util");
}

fn setup_workspace_partial_script_fixture(workspace: &Path, root_script: &str) {
    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create lib dir");

    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "private": true,
            "scripts": {
                "entry": root_script
            }
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
            "scripts": {}
        })
        .to_string(),
    )
    .expect("write lib package.json");

    write_bin_append_line_script(workspace, "app-mark", "app");
}

fn has_mark(dir: &Path) -> bool {
    dir.join("run-order.txt").exists()
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

#[test]
fn run_should_translate_embedded_pnpm_filter_run_to_internal_logic() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "private": true,
            "scripts": {
                "lint": "pnpm --filter ep_etherpad-lite run lint"
            }
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "ep_etherpad-lite",
            "version": "1.0.0",
            "scripts": {
                "lint": "app-lint"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");

    write_bin_append_line_script(&workspace, "app-lint", "linted");

    pacquet.with_args(["run", "lint"]).assert().success();

    assert_eq!(
        fs::read_to_string(app_dir.join("run-order.txt")).expect("read run-order.txt").trim(),
        "linted"
    );
    drop(root);
}

#[test]
fn run_filter_dependencies_selector_should_include_transitive_dependencies() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "pnpm --filter app... run mark", false);

    pacquet.with_args(["run", "mark"]).assert().success();

    assert!(has_mark(&workspace.join("packages/app")));
    assert!(has_mark(&workspace.join("packages/lib")));
    assert!(has_mark(&workspace.join("packages/util")));
    drop(root);
}

#[test]
fn run_filter_dependents_selector_should_include_transitive_dependents() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "pnpm --filter ...util run mark", false);

    pacquet.with_args(["run", "mark"]).assert().success();

    assert!(has_mark(&workspace.join("packages/app")));
    assert!(has_mark(&workspace.join("packages/lib")));
    assert!(has_mark(&workspace.join("packages/util")));
    drop(root);
}

#[test]
fn run_filter_prod_should_ignore_dev_dependencies_for_graph_traversal() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "pnpm --filter-prod app... run mark", true);

    pacquet.with_args(["run", "mark"]).assert().success();

    assert!(has_mark(&workspace.join("packages/app")));
    assert!(!has_mark(&workspace.join("packages/lib")));
    assert!(!has_mark(&workspace.join("packages/util")));
    drop(root);
}

#[test]
fn run_direct_filter_prod_should_ignore_dev_dependencies_for_graph_traversal() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "root-mark", true);

    pacquet.with_args(["run", "--filter-prod", "app...", "mark"]).assert().success();

    assert!(has_mark(&workspace.join("packages/app")));
    assert!(!has_mark(&workspace.join("packages/lib")));
    assert!(!has_mark(&workspace.join("packages/util")));
    drop(root);
}

#[test]
fn run_filter_exclude_selector_should_remove_matches() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(
        &workspace,
        "pnpm --filter app --filter lib --filter !lib run mark",
        false,
    );

    pacquet.with_args(["run", "mark"]).assert().success();

    assert!(has_mark(&workspace.join("packages/app")));
    assert!(!has_mark(&workspace.join("packages/lib")));
    assert!(!has_mark(&workspace.join("packages/util")));
    drop(root);
}

#[test]
fn run_filter_fail_if_no_match_should_fail() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(
        &workspace,
        "pnpm --filter does-not-exist --fail-if-no-match run mark",
        false,
    );

    pacquet.with_args(["run", "mark"]).assert().failure();
    drop(root);
}

#[test]
fn run_direct_fail_if_no_match_should_fail() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "root-mark", false);

    pacquet
        .with_args(["run", "--filter", "does-not-exist", "--fail-if-no-match", "mark"])
        .assert()
        .failure();
    drop(root);
}

#[test]
fn run_filter_should_skip_packages_without_script_when_some_match() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_partial_script_fixture(&workspace, "pnpm --filter app --filter lib run mark");

    pacquet.with_args(["run", "entry"]).assert().success();

    assert!(has_mark(&workspace.join("packages/app")));
    assert!(!has_mark(&workspace.join("packages/lib")));
    drop(root);
}

#[test]
fn run_recursive_resume_from_should_start_from_selected_package() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(
        &workspace,
        "pnpm -r --no-sort --resume-from lib run mark",
        false,
    );

    pacquet.with_args(["run", "mark"]).assert().success();

    assert!(!has_mark(&workspace.join("packages/app")));
    assert!(has_mark(&workspace.join("packages/lib")));
    assert!(has_mark(&workspace.join("packages/util")));
    drop(root);
}

#[test]
fn run_recursive_resume_from_should_fail_when_package_is_missing() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(
        &workspace,
        "pnpm -r --resume-from does-not-exist run mark",
        false,
    );

    pacquet.with_args(["run", "mark"]).assert().failure();
    drop(root);
}

#[test]
fn run_recursive_workspace_concurrency_flag_should_be_supported() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "pnpm -r --workspace-concurrency=1 run mark", false);

    pacquet.with_args(["run", "mark"]).assert().success();

    assert!(has_mark(&workspace.join("packages/app")));
    assert!(has_mark(&workspace.join("packages/lib")));
    assert!(has_mark(&workspace.join("packages/util")));
    drop(root);
}

#[test]
fn run_recursive_flag_should_target_workspace_packages_and_exclude_root() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create lib dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "private": true,
            "scripts": {
                "mark": "root-mark"
            }
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "scripts": { "mark": "app-mark" }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "lib",
            "version": "1.0.0",
            "scripts": { "mark": "lib-mark" }
        })
        .to_string(),
    )
    .expect("write lib package.json");
    write_bin_append_line_script(&workspace, "root-mark", "root");
    write_bin_append_line_script(&workspace, "app-mark", "app");
    write_bin_append_line_script(&workspace, "lib-mark", "lib");

    pacquet.with_args(["run", "-r", "mark"]).assert().success();

    assert!(!workspace.join("run-order.txt").exists());
    assert!(has_mark(&app_dir));
    assert!(has_mark(&lib_dir));
    drop(root);
}

#[test]
fn run_recursive_flag_from_subproject_should_still_target_workspace_packages() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create lib dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "private": true,
            "scripts": {
                "mark": "root-mark"
            }
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "scripts": { "mark": "app-mark" }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "lib",
            "version": "1.0.0",
            "scripts": { "mark": "lib-mark" }
        })
        .to_string(),
    )
    .expect("write lib package.json");
    write_bin_append_line_script(&workspace, "root-mark", "root");
    write_bin_append_line_script(&workspace, "app-mark", "app");
    write_bin_append_line_script(&workspace, "lib-mark", "lib");

    pacquet
        .with_args(["-C", app_dir.to_str().unwrap(), "run", "--recursive", "mark"])
        .assert()
        .success();

    assert!(!workspace.join("run-order.txt").exists());
    assert!(has_mark(&app_dir));
    assert!(has_mark(&lib_dir));
    drop(root);
}

#[test]
fn run_parallel_should_prefix_output_by_default() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create lib dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "private": true,
            "scripts": {
                "entry": "pnpm -r --parallel run mark"
            }
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "scripts": { "mark": "app-echo" }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "lib",
            "version": "1.0.0",
            "scripts": { "mark": "lib-echo" }
        })
        .to_string(),
    )
    .expect("write lib package.json");
    write_bin_echo_script(&workspace, "app-echo", "app-out");
    write_bin_echo_script(&workspace, "lib-echo", "lib-out");

    let assert = pacquet.with_args(["run", "entry"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(stdout.contains("app mark: app-out"));
    assert!(stdout.contains("lib mark: lib-out"));
    drop(root);
}

#[test]
fn run_parallel_reporter_hide_prefix_should_hide_prefixes() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create lib dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "private": true,
            "scripts": {
                "entry": "pnpm -r --parallel --reporter-hide-prefix run mark"
            }
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "scripts": { "mark": "app-echo" }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "lib",
            "version": "1.0.0",
            "scripts": { "mark": "lib-echo" }
        })
        .to_string(),
    )
    .expect("write lib package.json");
    write_bin_echo_script(&workspace, "app-echo", "app-out");
    write_bin_echo_script(&workspace, "lib-echo", "lib-out");

    let assert = pacquet.with_args(["run", "entry"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(stdout.contains("app-out"));
    assert!(stdout.contains("lib-out"));
    assert!(!stdout.contains("app mark:"));
    assert!(!stdout.contains("lib mark:"));
    drop(root);
}

#[test]
fn run_direct_parallel_flags_should_prefix_output_by_default() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
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
            "scripts": { "mark": "app-echo" }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "lib",
            "version": "1.0.0",
            "scripts": { "mark": "lib-echo" }
        })
        .to_string(),
    )
    .expect("write lib package.json");
    write_bin_echo_script(&workspace, "app-echo", "app-out");
    write_bin_echo_script(&workspace, "lib-echo", "lib-out");

    let assert = pacquet.with_args(["run", "--recursive", "--parallel", "mark"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(stdout.contains("app mark: app-out"));
    assert!(stdout.contains("lib mark: lib-out"));
    drop(root);
}

#[test]
fn run_direct_parallel_reporter_hide_prefix_should_hide_prefixes() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
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
            "scripts": { "mark": "app-echo" }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "lib",
            "version": "1.0.0",
            "scripts": { "mark": "lib-echo" }
        })
        .to_string(),
    )
    .expect("write lib package.json");
    write_bin_echo_script(&workspace, "app-echo", "app-out");
    write_bin_echo_script(&workspace, "lib-echo", "lib-out");

    let assert = pacquet
        .with_args(["run", "--recursive", "--parallel", "--reporter-hide-prefix", "mark"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(stdout.contains("app-out"));
    assert!(stdout.contains("lib-out"));
    assert!(!stdout.contains("app mark:"));
    assert!(!stdout.contains("lib mark:"));
    drop(root);
}

#[test]
fn run_should_route_nested_pnpm_invocations_to_pacquet() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {
                "check": "pnpm --version > nested-pnpm-version.txt"
            }
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet.with_args(["run", "check"]).assert().success();

    let version = fs::read_to_string(workspace.join("nested-pnpm-version.txt"))
        .expect("read nested-pnpm-version.txt");
    assert!(version.trim().starts_with("pacquet "));
    drop(root);
}

#[test]
fn run_workspace_root_flag_should_include_root_with_filters() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let app_dir = workspace.join("packages/app");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace-root",
            "private": true,
            "scripts": {
                "entry": "pnpm --workspace-root --filter app run mark",
                "mark": "root-mark"
            }
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
    write_bin_append_line_script(&workspace, "root-mark", "root");
    write_bin_append_line_script(&workspace, "app-mark", "app");

    pacquet.with_args(["run", "entry"]).assert().success();

    assert!(workspace.join("run-order.txt").exists());
    assert!(has_mark(&workspace.join("packages/app")));
    drop(root);
}

#[test]
fn run_report_summary_should_write_pnpm_exec_summary_json() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "pnpm -r --report-summary run mark", false);

    pacquet.with_args(["run", "mark"]).assert().success();

    let summary_path = workspace.join("pnpm-exec-summary.json");
    assert!(summary_path.exists());
    let summary = fs::read_to_string(summary_path).expect("read summary");
    let summary: serde_json::Value = serde_json::from_str(&summary).expect("parse summary");
    assert_eq!(
        summary
            .get("executionStatus")
            .and_then(|status| status.get("packages/app"))
            .and_then(|entry| entry.get("status"))
            .and_then(|status| status.as_str()),
        Some("passed")
    );
    drop(root);
}

#[test]
fn run_direct_report_summary_should_write_pnpm_exec_summary_json() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "root-mark", false);

    pacquet.with_args(["run", "--recursive", "--report-summary", "mark"]).assert().success();

    let summary_path = workspace.join("pnpm-exec-summary.json");
    assert!(summary_path.exists());
    let summary = fs::read_to_string(summary_path).expect("read summary");
    let summary: serde_json::Value = serde_json::from_str(&summary).expect("parse summary");
    assert_eq!(
        summary
            .get("executionStatus")
            .and_then(|status| status.get("packages/app"))
            .and_then(|entry| entry.get("status"))
            .and_then(|status| status.as_str()),
        Some("passed")
    );
    drop(root);
}

#[test]
fn run_direct_resume_from_should_start_from_selected_package() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "root-mark", false);

    pacquet
        .with_args(["run", "--recursive", "--no-sort", "--resume-from", "lib", "mark"])
        .assert()
        .success();

    assert!(!has_mark(&workspace.join("packages/app")));
    assert!(has_mark(&workspace.join("packages/lib")));
    assert!(has_mark(&workspace.join("packages/util")));
    drop(root);
}

#[test]
fn run_direct_workspace_concurrency_should_be_supported() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "root-mark", false);

    pacquet
        .with_args(["run", "--recursive", "--workspace-concurrency", "1", "mark"])
        .assert()
        .success();

    assert!(has_mark(&workspace.join("packages/app")));
    assert!(has_mark(&workspace.join("packages/lib")));
    assert!(has_mark(&workspace.join("packages/util")));
    drop(root);
}

#[test]
fn run_direct_sequential_should_be_supported() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "root-mark", false);

    pacquet.with_args(["run", "--recursive", "--sequential", "mark"]).assert().success();

    assert!(has_mark(&workspace.join("packages/app")));
    assert!(has_mark(&workspace.join("packages/lib")));
    assert!(has_mark(&workspace.join("packages/util")));
    drop(root);
}

#[test]
fn run_direct_reverse_should_be_supported() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "root-mark", false);

    pacquet.with_args(["run", "--recursive", "--reverse", "mark"]).assert().success();

    assert!(has_mark(&workspace.join("packages/app")));
    assert!(has_mark(&workspace.join("packages/lib")));
    assert!(has_mark(&workspace.join("packages/util")));
    drop(root);
}

#[test]
fn run_direct_stream_flag_should_be_accepted() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    setup_workspace_filter_fixture(&workspace, "root-mark", false);

    pacquet.with_args(["run", "--recursive", "--stream", "mark"]).assert().success();

    assert!(has_mark(&workspace.join("packages/app")));
    assert!(has_mark(&workspace.join("packages/lib")));
    assert!(has_mark(&workspace.join("packages/util")));
    drop(root);
}

#[test]
fn run_direct_no_bail_should_continue_after_failure() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let fail_dir = workspace.join("packages/a-fail");
    let pass_dir = workspace.join("packages/z-pass");
    fs::create_dir_all(&fail_dir).expect("create fail dir");
    fs::create_dir_all(&pass_dir).expect("create pass dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(workspace.join("package.json"), serde_json::json!({ "private": true }).to_string())
        .expect("write root package.json");
    fs::write(
        fail_dir.join("package.json"),
        serde_json::json!({
            "name": "a-fail",
            "version": "1.0.0",
            "scripts": { "mark": "fail-mark" }
        })
        .to_string(),
    )
    .expect("write fail package.json");
    fs::write(
        pass_dir.join("package.json"),
        serde_json::json!({
            "name": "z-pass",
            "version": "1.0.0",
            "scripts": { "mark": "pass-mark" }
        })
        .to_string(),
    )
    .expect("write pass package.json");
    write_bin_failure_script(&workspace, "fail-mark");
    write_bin_append_line_script(&workspace, "pass-mark", "pass");

    pacquet.with_args(["run", "--recursive", "--no-sort", "--no-bail", "mark"]).assert().success();

    assert!(has_mark(&pass_dir));
    drop(root);
}

#[test]
fn run_direct_bail_should_stop_on_failure() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let fail_dir = workspace.join("packages/a-fail");
    let pass_dir = workspace.join("packages/z-pass");
    fs::create_dir_all(&fail_dir).expect("create fail dir");
    fs::create_dir_all(&pass_dir).expect("create pass dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(workspace.join("package.json"), serde_json::json!({ "private": true }).to_string())
        .expect("write root package.json");
    fs::write(
        fail_dir.join("package.json"),
        serde_json::json!({
            "name": "a-fail",
            "version": "1.0.0",
            "scripts": { "mark": "fail-mark" }
        })
        .to_string(),
    )
    .expect("write fail package.json");
    fs::write(
        pass_dir.join("package.json"),
        serde_json::json!({
            "name": "z-pass",
            "version": "1.0.0",
            "scripts": { "mark": "pass-mark" }
        })
        .to_string(),
    )
    .expect("write pass package.json");
    write_bin_failure_script(&workspace, "fail-mark");
    write_bin_append_line_script(&workspace, "pass-mark", "pass");

    pacquet.with_args(["run", "--recursive", "--no-sort", "--bail", "mark"]).assert().failure();

    assert!(!has_mark(&pass_dir));
    drop(root);
}
