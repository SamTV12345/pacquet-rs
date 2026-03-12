pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::{
    bin::{AddMockedRegistry, CommandTempCwd},
    fs::{get_filenames_in_folder, symlink_or_junction_target},
};
use std::{fs, path::PathBuf};

#[test]
fn dlx_should_run_default_bin_from_temporary_package() {
    let CommandTempCwd { root, pacquet, .. } = CommandTempCwd::init().add_mocked_registry();

    let assert =
        pacquet.with_args(["dlx", "@pnpm.e2e/hello-world-js-bin@1.0.0"]).assert().success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("Hello world!"));

    drop(root);
}

#[test]
fn dlx_reporter_silent_should_keep_command_output_but_hide_install_output() {
    let CommandTempCwd { root, pacquet, .. } = CommandTempCwd::init().add_mocked_registry();

    let assert = pacquet
        .with_args(["dlx", "--reporter", "silent", "@pnpm.e2e/hello-world-js-bin@1.0.0"])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(stdout.contains("Hello world!"));
    assert!(!stdout.contains("Packages: +"));
    assert!(stderr.trim().is_empty());

    drop(root);
}

#[test]
fn dlx_shell_mode_should_run_installed_command_in_current_working_dir() {
    let CommandTempCwd { root, workspace, pacquet, .. } =
        CommandTempCwd::init().add_mocked_registry();

    pacquet
        .with_args([
            "dlx",
            "--package",
            "@pnpm.e2e/hello-world-js-bin@1.0.0",
            "--shell-mode",
            "hello-world-js-bin > dlx-output.txt",
        ])
        .assert()
        .success();

    let output = fs::read_to_string(workspace.join("dlx-output.txt")).expect("read dlx output");
    assert!(output.contains("Hello world!"));

    drop(root);
}

#[test]
fn dlx_should_reuse_fresh_cache_directory() {
    let CommandTempCwd { root, pacquet, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { cache_dir, .. } = npmrc_info;

    pacquet.with_args(["dlx", "@pnpm.e2e/hello-world-js-bin@1.0.0"]).assert().success();
    let first_target = cached_dlx_target_dir(&cache_dir);

    #[allow(deprecated)]
    let pacquet_second = std::process::Command::cargo_bin("pacquet")
        .expect("find pacquet binary")
        .with_current_dir(root.path().join("workspace"));
    pacquet_second.with_args(["dlx", "@pnpm.e2e/hello-world-js-bin@1.0.0"]).assert().success();
    let second_target = cached_dlx_target_dir(&cache_dir);

    assert_eq!(first_target, second_target);
    let key_dir = first_target.parent().expect("cache key dir");
    let mut entries = get_filenames_in_folder(key_dir);
    entries.retain(|name| name != "pkg");
    assert_eq!(entries.len(), 1);

    drop(root);
}

#[test]
fn dlx_cache_max_age_zero_should_create_new_prepare_dir() {
    let CommandTempCwd { root, workspace, pacquet, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { cache_dir, npmrc_path, .. } = npmrc_info;
    fs::write(
        &npmrc_path,
        format!("{}\ndlx-cache-max-age=0\n", fs::read_to_string(&npmrc_path).expect("read npmrc")),
    )
    .expect("rewrite npmrc");

    pacquet.with_args(["dlx", "@pnpm.e2e/hello-world-js-bin@1.0.0"]).assert().success();
    let first_target = cached_dlx_target_dir(&cache_dir);

    #[allow(deprecated)]
    let pacquet_second = std::process::Command::cargo_bin("pacquet")
        .expect("find pacquet binary")
        .with_current_dir(&workspace);
    pacquet_second.with_args(["dlx", "@pnpm.e2e/hello-world-js-bin@1.0.0"]).assert().success();
    let second_target = cached_dlx_target_dir(&cache_dir);

    assert_ne!(first_target, second_target);
    let key_dir = second_target.parent().expect("cache key dir");
    let mut entries = get_filenames_in_folder(key_dir);
    entries.retain(|name| name != "pkg");
    assert_eq!(entries.len(), 2);

    drop(root);
}

fn cached_dlx_target_dir(cache_dir: &std::path::Path) -> PathBuf {
    let dlx_root = cache_dir.join("dlx");
    let [key_dir] = get_filenames_in_folder(&dlx_root).try_into().expect("one cache key dir");
    let key_dir = dlx_root.join(key_dir);
    let cache_link = key_dir.join("pkg");
    symlink_or_junction_target(&cache_link).expect("read dlx cache link target")
}
