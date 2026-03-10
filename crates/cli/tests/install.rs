pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::{
    bin::{AddMockedRegistry, CommandTempCwd},
    fs::{get_all_files, get_all_folders, is_symlink_or_junction},
};
use std::{
    fs,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

#[cfg(not(target_os = "windows"))] // It causes ConnectionAborted on CI
#[cfg(not(target_os = "macos"))] // It causes ConnectionReset on CI
use pacquet_testing_utils::fixtures::{BIG_LOCKFILE, BIG_MANIFEST};

fn normalize_store_files_for_snapshot(files: Vec<String>) -> Vec<String> {
    files.into_iter().filter(|path| !path.starts_with("v10/projects/")).collect()
}

fn installed_bin_path(workspace: &std::path::Path, name: &str) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        workspace.join("node_modules/.bin").join(format!("{name}.cmd"))
    }

    #[cfg(not(windows))]
    {
        workspace.join("node_modules/.bin").join(name)
    }
}

fn metadata_cache_file(cache_dir: &Path, registry: &str, package_name: &str) -> std::path::PathBuf {
    let trimmed = registry.trim_end_matches('/');
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let registry_namespace = without_scheme.replace(':', "+");
    let encoded_name = package_name.replace('/', "%2f");
    cache_dir.join("metadata-v1.3").join(registry_namespace).join(format!("{encoded_name}.json"))
}

fn path_exists_or_is_link(path: &Path) -> bool {
    path.exists() || is_symlink_or_junction(path).unwrap_or(false)
}

fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        if path_exists_or_is_link(path) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn should_install_dependencies() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Executing command...");
    pacquet.with_arg("install").assert().success();

    eprintln!("Make sure the package is installed");
    let symlink_path = workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent");
    assert!(is_symlink_or_junction(&symlink_path).unwrap());
    let virtual_path =
        workspace.join("node_modules/.pnpm/@pnpm.e2e+hello-world-js-bin-parent@1.0.0");
    assert!(virtual_path.exists());

    eprintln!("Make sure it installs direct dependencies");
    assert!(!workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists());
    assert!(workspace.join("node_modules/.pnpm/@pnpm.e2e+hello-world-js-bin@1.0.0").exists());

    eprintln!("Snapshot");
    let workspace_folders = get_all_folders(&workspace);
    let store_files = normalize_store_files_for_snapshot(get_all_files(&store_dir));
    insta::assert_debug_snapshot!((workspace_folders, store_files));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn scoped_registry_should_override_default_registry_for_install_and_metadata_cache() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { npmrc_path, store_dir: _, cache_dir, mock_instance } = npmrc_info;

    fs::write(
        &npmrc_path,
        format!(
            "registry=http://127.0.0.1:9/\n@pnpm.e2e:registry={}\nstore-dir=../pacquet-store\ncache-dir=../pacquet-cache\n",
            mock_instance.url()
        ),
    )
    .expect("rewrite .npmrc with scoped registry");

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin": "1.0.0",
            },
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet.with_arg("install").assert().success();

    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists());
    assert!(
        metadata_cache_file(&cache_dir, &mock_instance.url(), "@pnpm.e2e/hello-world-js-bin")
            .exists()
    );

    drop((root, mock_instance)); // cleanup
}

#[test]
fn frozen_lockfile_should_use_scoped_registry_and_omit_tarball_url_by_default() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { npmrc_path, mock_instance, .. } = npmrc_info;

    fs::write(
        &npmrc_path,
        format!(
            "registry=http://127.0.0.1:9/\n@pnpm.e2e:registry={}\nstore-dir=../pacquet-store\ncache-dir=../pacquet-cache\n",
            mock_instance.url()
        ),
    )
    .expect("rewrite .npmrc with scoped registry");

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin": "1.0.0",
            },
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_args(["install", "--lockfile-only"]).assert().success();

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read lockfile");
    assert!(!lockfile_content.contains("tarball:"));

    pacquet_command(&workspace).with_args(["install", "--frozen-lockfile"]).assert().success();
    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn installed_dependency_bin_should_be_runnable_via_pacquet_run() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "name": "app",
        "version": "1.0.0",
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin": "1.0.0",
        },
        "scripts": {
            "hello": "hello-world-js-bin"
        }
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_arg("install").assert().success();
    assert!(installed_bin_path(&workspace, "hello-world-js-bin").exists());

    #[allow(deprecated)]
    let mut run = std::process::Command::cargo_bin("pacquet").expect("find the pacquet binary");
    run.current_dir(&workspace);
    run.with_args(["run", "hello"]).assert().success();

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_install_exec_files() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Executing command...");
    pacquet.with_arg("install").assert().success();

    eprintln!("Listing all files in the store...");
    let store_files = normalize_store_files_for_snapshot(get_all_files(&store_dir));

    #[cfg(unix)]
    {
        use pacquet_testing_utils::fs::is_path_executable;
        use pipe_trait::Pipe;
        use pretty_assertions::assert_eq;
        use std::{fs::File, iter::repeat, os::unix::fs::MetadataExt};

        eprintln!("All files that end with '-exec' are executable, others not");
        let (suffix_exec, suffix_other) =
            store_files.iter().partition::<Vec<_>, _>(|path| path.ends_with("-exec"));
        let (mode_exec, mode_other) = store_files
            .iter()
            .partition::<Vec<_>, _>(|name| store_dir.join(name).as_path().pipe(is_path_executable));
        assert_eq!((&suffix_exec, &suffix_other), (&mode_exec, &mode_other));

        eprintln!("All executable files have mode 755");
        let actual_modes: Vec<_> = mode_exec
            .iter()
            .map(|name| {
                let mode = store_dir
                    .join(name)
                    .pipe(File::open)
                    .expect("open file to get mode")
                    .metadata()
                    .expect("get metadata")
                    .mode();
                (name.as_str(), mode & 0o777)
            })
            .collect();
        let expected_modes: Vec<_> =
            mode_exec.iter().map(|name| name.as_str()).zip(repeat(0o755)).collect();
        assert_eq!(&actual_modes, &expected_modes);
    }

    eprintln!("Snapshot");
    insta::assert_debug_snapshot!(store_files);

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_install_index_files() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Executing command...");
    pacquet.with_arg("install").assert().success();

    eprintln!("Snapshot");
    let index_file_contents = index_file_contents(&store_dir);
    insta::assert_yaml_snapshot!(index_file_contents);

    drop((root, mock_instance)); // cleanup
}

#[cfg(not(target_os = "windows"))] // It causes ConnectionAborted on CI
#[cfg(not(target_os = "macos"))] // It causes ConnectionReset on CI
#[test]
fn frozen_lockfile_should_be_able_to_handle_big_lockfile() {
    use std::{fs::OpenOptions, io::Write};

    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    fs::write(manifest_path, BIG_MANIFEST).expect("write to package.json");

    eprintln!("Creating pnpm-lock.yaml...");
    let lockfile_path = workspace.join("pnpm-lock.yaml");
    fs::write(lockfile_path, BIG_LOCKFILE).expect("write to pnpm-lock.yaml");

    eprintln!("Patching .npmrc...");
    let npmrc_path = workspace.join(".npmrc");
    OpenOptions::new()
        .append(true)
        .write(true)
        .open(npmrc_path)
        .expect("open .npmrc to append")
        .write_all(b"\nlockfile=true\nauto-install-peers=true\n")
        .expect("append to .npmrc");

    eprintln!("Executing command...");
    pacquet.with_args(["install", "--frozen-lockfile"]).assert().success();

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_install_circular_dependencies() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/circular-deps-1-of-2": "1.0.2",
        },
    });
    fs::write(manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Executing command...");
    pacquet.with_arg("install").assert().success();

    assert!(workspace.join("./node_modules/@pnpm.e2e/circular-deps-1-of-2").exists());
    assert!(workspace.join("./node_modules/.pnpm/@pnpm.e2e+circular-deps-1-of-2@1.0.2").exists());
    assert!(workspace.join("./node_modules/.pnpm/@pnpm.e2e+circular-deps-2-of-2@1.0.2").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_create_lockfile_by_default() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_arg("install").assert().success();

    assert!(workspace.join("pnpm-lock.yaml").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn lockfile_should_not_include_tarball_url_by_default() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_arg("install").assert().success();

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read lockfile");
    assert!(!lockfile_content.contains("tarball:"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn lockfile_should_include_tarball_url_when_enabled_in_npmrc() {
    use std::io::Write;

    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");
    fs::OpenOptions::new()
        .append(true)
        .open(workspace.join(".npmrc"))
        .expect("open .npmrc")
        .write_all(b"\nlockfile-include-tarball-url=true\n")
        .expect("append lockfile-include-tarball-url=true");

    pacquet.with_arg("install").assert().success();

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read lockfile");
    assert!(lockfile_content.contains("tarball:"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn frozen_lockfile_should_fail_without_pnpm_lock_yaml() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_args(["install", "--frozen-lockfile"]).assert().failure();
    assert!(!workspace.join("pnpm-lock.yaml").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn fix_lockfile_should_override_frozen_lockfile_when_lockfile_is_missing() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_args(["install", "--frozen-lockfile", "--fix-lockfile"]).assert().success();
    assert!(workspace.join("pnpm-lock.yaml").exists());
    assert!(wait_for_path(
        &workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent"),
        Duration::from_secs(2),
    ));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn install_should_succeed_when_prefer_frozen_lockfile_is_disabled() {
    use std::io::Write;

    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet_command(&workspace).with_args(["install"]).assert().success();

    fs::OpenOptions::new()
        .append(true)
        .open(workspace.join(".npmrc"))
        .expect("open .npmrc")
        .write_all(b"\nprefer-frozen-lockfile=false\n")
        .expect("append prefer-frozen-lockfile=false");

    pacquet_command(&workspace).with_args(["install"]).assert().success();
    assert!(workspace.join("pnpm-lock.yaml").exists());
    assert!(wait_for_path(
        &workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent"),
        Duration::from_secs(2),
    ));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn frozen_lockfile_flag_should_work_when_prefer_frozen_lockfile_is_disabled() {
    use std::io::Write;

    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");
    pacquet_command(&workspace).with_args(["install"]).assert().success();

    fs::OpenOptions::new()
        .append(true)
        .open(workspace.join(".npmrc"))
        .expect("open .npmrc")
        .write_all(b"\nprefer-frozen-lockfile=false\n")
        .expect("append prefer-frozen-lockfile=false");

    pacquet_command(&workspace).with_args(["install", "--frozen-lockfile"]).assert().success();
    assert!(wait_for_path(
        &workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent"),
        Duration::from_secs(2),
    ));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn prefer_frozen_lockfile_flag_should_override_npmrc_false_setting() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, cache_dir, mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet_command(&workspace).with_args(["install"]).assert().success();

    fs::write(
        workspace.join(".npmrc"),
        format!(
            "registry=http://127.0.0.1:9/\nstore-dir={}\ncache-dir={}\nprefer-frozen-lockfile=false\nfetch-timeout=100\n",
            store_dir.display(),
            cache_dir.display()
        ),
    )
    .expect("rewrite .npmrc");

    let cached_metadata = metadata_cache_file(
        &cache_dir,
        &mock_instance.url(),
        "@pnpm.e2e/hello-world-js-bin-parent",
    );
    if cached_metadata.exists() {
        fs::remove_file(&cached_metadata).expect("remove cached metadata");
    }

    pacquet_command(&workspace)
        .with_args(["install", "--prefer-frozen-lockfile"])
        .assert()
        .success();
    pacquet_command(&workspace).with_args(["install"]).assert().failure();

    drop(root); // cleanup
}

#[test]
fn no_prefer_frozen_lockfile_flag_should_override_default_true_setting() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, cache_dir, mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet_command(&workspace).with_args(["install"]).assert().success();

    let cached_metadata = metadata_cache_file(
        &cache_dir,
        &mock_instance.url(),
        "@pnpm.e2e/hello-world-js-bin-parent",
    );
    if cached_metadata.exists() {
        fs::remove_file(&cached_metadata).expect("remove cached metadata");
    }

    fs::write(
        workspace.join(".npmrc"),
        format!(
            "registry=http://127.0.0.1:9/\nstore-dir={}\ncache-dir={}\nfetch-timeout=100\n",
            store_dir.display(),
            cache_dir.display()
        ),
    )
    .expect("rewrite .npmrc");

    pacquet_command(&workspace).with_args(["install"]).assert().success();
    pacquet_command(&workspace)
        .with_args(["install", "--no-prefer-frozen-lockfile"])
        .assert()
        .failure();

    drop(root); // cleanup
}

#[test]
fn should_accept_ignore_scripts_flag() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_args(["install", "--ignore-scripts"]).assert().success();

    assert!(wait_for_path(
        &workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent"),
        Duration::from_secs(2),
    ));
    assert!(workspace.join("pnpm-lock.yaml").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn symlink_false_with_hoisted_linker_should_copy_package_into_node_modules() {
    use std::io::Write;

    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");
    fs::OpenOptions::new()
        .append(true)
        .open(workspace.join(".npmrc"))
        .expect("open .npmrc")
        .write_all(b"\nsymlink=false\nnode-linker=hoisted\n")
        .expect("append symlink/node-linker settings");

    pacquet.with_args(["install"]).assert().success();

    let package_path = workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent");
    assert!(package_path.exists());
    assert!(!is_symlink_or_junction(&package_path).expect("read package metadata"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn symlink_false_with_isolated_linker_should_not_link_direct_dependency_at_root() {
    use std::io::Write;

    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");
    fs::OpenOptions::new()
        .append(true)
        .open(workspace.join(".npmrc"))
        .expect("open .npmrc")
        .write_all(b"\nsymlink=false\nnode-linker=isolated\n")
        .expect("append symlink/node-linker settings");

    pacquet.with_args(["install"]).assert().success();

    let package_path = workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent");
    assert!(!package_path.exists());
    assert!(
        workspace
            .join("node_modules/.pnpm/@pnpm.e2e+hello-world-js-bin-parent@1.0.0/node_modules/@pnpm.e2e/hello-world-js-bin-parent")
            .exists()
    );

    drop((root, mock_instance)); // cleanup
}

#[test]
fn strict_peer_dependencies_should_fail_for_missing_peer_on_link_dependency() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let dep_dir = workspace.join("a");
    fs::create_dir_all(&dep_dir).expect("create dependency dir");
    fs::write(
        dep_dir.join("package.json"),
        serde_json::json!({
            "name": "a",
            "version": "1.0.0",
            "peerDependencies": {
                "peer-a": "^1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write dependency package.json");

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "dependencies": {
                "a": "link:./a"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    fs::write(workspace.join(".npmrc"), "strict-peer-dependencies=true\n").expect("write .npmrc");

    pacquet.with_args(["install"]).assert().failure();

    drop(root); // cleanup
}

#[test]
fn strict_peer_dependencies_should_succeed_when_peer_is_present_on_link_dependency() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let dep_dir = workspace.join("a");
    let peer_dir = workspace.join("peer");
    fs::create_dir_all(&dep_dir).expect("create dependency dir");
    fs::create_dir_all(&peer_dir).expect("create peer dir");
    fs::write(
        dep_dir.join("package.json"),
        serde_json::json!({
            "name": "a",
            "version": "1.0.0",
            "peerDependencies": {
                "peer-a": "^1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write dependency package.json");
    fs::write(
        peer_dir.join("package.json"),
        serde_json::json!({
            "name": "peer-a",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write peer package.json");

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "dependencies": {
                "a": "link:./a",
                "peer-a": "link:./peer"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    fs::write(workspace.join(".npmrc"), "strict-peer-dependencies=true\n").expect("write .npmrc");

    pacquet.with_args(["install"]).assert().success();
    assert!(workspace.join("node_modules/a").exists());
    assert!(workspace.join("node_modules/peer-a").exists());

    drop(root); // cleanup
}

#[test]
fn strict_peer_dependencies_should_resolve_peer_from_workspace_root_by_default() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let dep_dir = workspace.join("a");
    let root_peer_dir = workspace.join("node_modules/peer-a");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&dep_dir).expect("create dependency dir");
    fs::create_dir_all(&root_peer_dir).expect("create root peer dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        dep_dir.join("package.json"),
        serde_json::json!({
            "name": "a",
            "version": "1.0.0",
            "peerDependencies": {
                "peer-a": "^1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write dependency package.json");
    fs::write(
        root_peer_dir.join("package.json"),
        serde_json::json!({
            "name": "peer-a",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write root peer package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "a": "link:../../a"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(app_dir.join(".npmrc"), "strict-peer-dependencies=true\n").expect("write .npmrc");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    drop(root); // cleanup
}

#[test]
fn strict_peer_dependencies_should_not_resolve_peer_from_workspace_root_when_disabled() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let dep_dir = workspace.join("a");
    let root_peer_dir = workspace.join("node_modules/peer-a");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&dep_dir).expect("create dependency dir");
    fs::create_dir_all(&root_peer_dir).expect("create root peer dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        dep_dir.join("package.json"),
        serde_json::json!({
            "name": "a",
            "version": "1.0.0",
            "peerDependencies": {
                "peer-a": "^1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write dependency package.json");
    fs::write(
        root_peer_dir.join("package.json"),
        serde_json::json!({
            "name": "peer-a",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write root peer package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "a": "link:../../a"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        app_dir.join(".npmrc"),
        "strict-peer-dependencies=true\nresolve-peers-from-workspace-root=false\n",
    )
    .expect("write .npmrc");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().failure();

    drop(root); // cleanup
}

#[test]
fn should_create_lockfile_without_node_modules_with_lockfile_only() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_args(["install", "--lockfile-only"]).assert().success();

    assert!(workspace.join("pnpm-lock.yaml").exists());
    assert!(!workspace.join("node_modules").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_create_lockfile_without_node_modules_with_resolution_only() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_args(["install", "--resolution-only"]).assert().success();

    assert!(workspace.join("pnpm-lock.yaml").exists());
    assert!(!workspace.join("node_modules").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn lockfile_only_should_fail_when_lockfile_is_disabled() {
    use std::io::Write;

    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");
    fs::OpenOptions::new()
        .append(true)
        .open(workspace.join(".npmrc"))
        .expect("open .npmrc")
        .write_all(b"\nlockfile=false\n")
        .expect("append lockfile=false");

    pacquet.with_args(["install", "--lockfile-only"]).assert().failure();
    assert!(!workspace.join("pnpm-lock.yaml").exists());
    assert!(!workspace.join("node_modules").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn resolution_only_should_fail_when_lockfile_is_disabled() {
    use std::io::Write;

    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");
    fs::OpenOptions::new()
        .append(true)
        .open(workspace.join(".npmrc"))
        .expect("open .npmrc")
        .write_all(b"\nlockfile=false\n")
        .expect("append lockfile=false");

    pacquet.with_args(["install", "--resolution-only"]).assert().failure();
    assert!(!workspace.join("pnpm-lock.yaml").exists());
    assert!(!workspace.join("node_modules").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_accept_prefer_offline_flag() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_args(["install", "--prefer-offline"]).assert().success();
    assert!(workspace.join("pnpm-lock.yaml").exists());
    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_accept_reporter_flag() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_args(["install", "--reporter", "append-only"]).assert().success();
    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_accept_use_store_server_flag() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_args(["install", "--use-store-server"]).assert().success();
    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn shamefully_hoist_flag_should_create_hoisted_links() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_args(["install", "--shamefully-hoist"]).assert().success();
    assert!(workspace.join("node_modules/.pnpm/node_modules").exists());
    assert!(
        workspace.join("node_modules/.pnpm/node_modules/@pnpm.e2e/hello-world-js-bin").exists()
    );
    assert!(
        workspace
            .join("node_modules/.pnpm/node_modules/@pnpm.e2e/hello-world-js-bin-parent")
            .exists()
    );
    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn public_hoist_pattern_should_hoist_matching_transitive_dependency_to_root_node_modules() {
    use std::io::Write;

    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");
    fs::OpenOptions::new()
        .append(true)
        .open(workspace.join(".npmrc"))
        .expect("open .npmrc")
        .write_all(b"\npublic-hoist-pattern=*hello-world-js-bin\n")
        .expect("append public-hoist-pattern");

    pacquet.with_args(["install"]).assert().success();

    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());
    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn prefer_offline_should_fallback_to_online_when_cached_metadata_is_stale() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { cache_dir, mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    let cache_file = metadata_cache_file(
        &cache_dir,
        &mock_instance.url(),
        "@pnpm.e2e/hello-world-js-bin-parent",
    );
    fs::create_dir_all(cache_file.parent().expect("metadata cache parent"))
        .expect("create metadata cache parent");
    fs::write(
        &cache_file,
        serde_json::json!({
            "name": "@pnpm.e2e/hello-world-js-bin-parent",
            "dist-tags": { "latest": "0.0.0" },
            "versions": {}
        })
        .to_string(),
    )
    .expect("write stale metadata cache");

    pacquet.with_args(["install", "--prefer-offline"]).assert().success();
    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());
    assert!(workspace.join("pnpm-lock.yaml").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn prefer_offline_should_use_metadata_cache_when_registry_is_unavailable() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_arg("install")
        .assert()
        .success();
    fs::remove_dir_all(workspace.join("node_modules")).expect("remove node_modules");
    fs::remove_file(workspace.join("pnpm-lock.yaml")).expect("remove lockfile");
    drop(mock_instance);

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--prefer-offline"])
        .assert()
        .success();
    assert!(workspace.join("pnpm-lock.yaml").exists());
    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());

    drop(root); // cleanup
}

#[test]
fn force_should_reinstall_and_repair_corrupted_virtual_store_package() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_arg("install")
        .assert()
        .success();

    let package_file = workspace.join(
        "node_modules/.pnpm/@pnpm.e2e+hello-world-js-bin-parent@1.0.0/node_modules/@pnpm.e2e/hello-world-js-bin-parent/package.json",
    );
    fs::write(&package_file, "{\"name\":\"corrupted\"}")
        .expect("corrupt package.json in virtual store");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_arg("install")
        .assert()
        .success();
    let after_normal =
        fs::read_to_string(&package_file).expect("read package file after normal install");
    assert!(after_normal.contains("corrupted"));

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--force"])
        .assert()
        .success();
    let after_force =
        fs::read_to_string(&package_file).expect("read package file after force install");
    assert!(!after_force.contains("corrupted"));
    assert!(after_force.contains("\"name\""));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn offline_should_fail_without_lockfile() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_args(["install", "--offline"]).assert().failure();
    assert!(!workspace.join("node_modules").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn offline_should_install_without_lockfile_when_cache_and_store_are_primed() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_arg("install")
        .assert()
        .success();
    fs::remove_dir_all(workspace.join("node_modules")).expect("remove node_modules");
    fs::remove_file(workspace.join("pnpm-lock.yaml")).expect("remove lockfile");
    drop(mock_instance);

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--offline"])
        .assert()
        .success();
    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());

    drop(root); // cleanup
}

#[test]
fn workspace_install_from_subproject_should_write_shared_lockfile() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, cache_dir, mock_instance, .. } = npmrc_info;

    let project_dir = workspace.join("packages/app");
    fs::create_dir_all(&project_dir).expect("create workspace project");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");

    let manifest_path = project_dir.join("package.json");
    let package_json_content = serde_json::json!({
        "name": "app",
        "version": "1.0.0",
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");
    fs::write(
        project_dir.join(".npmrc"),
        format!(
            "registry={}\nstore-dir={}\ncache-dir={}\n",
            mock_instance.url(),
            store_dir.display(),
            cache_dir.display()
        ),
    )
    .expect("write .npmrc for subproject");

    pacquet.with_args(["-C", project_dir.to_str().unwrap(), "install"]).assert().success();

    let root_lockfile = workspace.join("pnpm-lock.yaml");
    assert!(root_lockfile.exists());
    assert!(!project_dir.join("pnpm-lock.yaml").exists());

    let lockfile_content = fs::read_to_string(root_lockfile).expect("read lockfile");
    assert!(lockfile_content.contains("importers:"));
    assert!(lockfile_content.contains("packages/app:"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_root_flag_should_install_workspace_root_manifest_from_subproject() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let app_dir = workspace.join("packages/app");
    fs::create_dir_all(&app_dir).expect("create workspace project");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace-root",
            "version": "1.0.0",
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write workspace root manifest");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "-w", "install"]).assert().success();

    assert!(wait_for_path(
        &workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent"),
        Duration::from_secs(2)
    ));
    assert!(workspace.join("pnpm-lock.yaml").exists());
    assert!(!app_dir.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_recursive_install_from_subproject_should_install_all_projects() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, cache_dir, mock_instance, .. } = npmrc_info;

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
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/app",
            "version": "1.0.0",
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write lib package.json");
    fs::write(
        app_dir.join(".npmrc"),
        format!(
            "registry={}\nstore-dir={}\ncache-dir={}\n",
            mock_instance.url(),
            store_dir.display(),
            cache_dir.display()
        ),
    )
    .expect("write .npmrc for subproject");

    pacquet
        .with_args(["-C", app_dir.to_str().unwrap(), "install", "--recursive"])
        .assert()
        .success();

    assert!(wait_for_path(
        &workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent"),
        Duration::from_secs(2)
    ));
    assert!(wait_for_path(
        &app_dir.join("node_modules/@pnpm.e2e/hello-world-js-bin"),
        Duration::from_secs(2)
    ));
    assert!(wait_for_path(
        &lib_dir.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent"),
        Duration::from_secs(2)
    ));

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("importers:"));
    assert!(lockfile_content.contains(".:"));
    assert!(lockfile_content.contains("packages/app:"));
    assert!(lockfile_content.contains("packages/lib:"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_filter_should_install_only_selected_workspace_project() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

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
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/app",
            "version": "1.0.0",
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write lib package.json");

    pacquet.with_args(["install", "--filter", "@repo/app"]).assert().success();

    assert!(app_dir.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());
    assert!(!lib_dir.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_filter_should_fail_when_no_projects_match() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    fs::create_dir_all(workspace.join("packages/app")).expect("create app dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace-root",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write root package.json");

    pacquet.with_args(["install", "--filter", "@repo/missing"]).assert().failure();

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_protocol_dependency_should_link_local_package() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app package dir");
    fs::create_dir_all(&lib_dir).expect("create lib package dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.2.3"
        })
        .to_string(),
    )
    .expect("write lib package manifest");
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
    .expect("write app package manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    let linked_dep = app_dir.join("node_modules/@repo/lib");
    assert!(linked_dep.exists());
    assert!(is_symlink_or_junction(&linked_dep).unwrap());

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("version: link:../lib"));

    drop(root); // cleanup
}

#[test]
fn workspace_protocol_dependency_should_inject_local_package_when_enabled() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app package dir");
    fs::create_dir_all(&lib_dir).expect("create lib package dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(app_dir.join(".npmrc"), "inject-workspace-packages=true\n").expect("write app npmrc");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.2.3",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write lib package manifest");
    fs::write(lib_dir.join("index.js"), "module.exports = 'lib';\n").expect("write lib entrypoint");
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
    .expect("write app package manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    let injected_dep = app_dir.join("node_modules/@repo/lib");
    assert!(injected_dep.exists());
    let metadata = fs::symlink_metadata(&injected_dep).expect("read injected dependency metadata");
    assert!(!metadata.file_type().is_symlink());
    assert!(injected_dep.join("package.json").exists());
    assert!(injected_dep.join("index.js").exists());

    drop(root); // cleanup
}

#[test]
fn workspace_protocol_dependency_should_inject_local_package_when_dependencies_meta_requests_it() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app package dir");
    fs::create_dir_all(&lib_dir).expect("create lib package dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.2.3",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write lib package manifest");
    fs::write(lib_dir.join("index.js"), "module.exports = 'lib';\n").expect("write lib entrypoint");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/lib": "workspace:*"
            },
            "dependenciesMeta": {
                "@repo/lib": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    let injected_dep = app_dir.join("node_modules/@repo/lib");
    assert!(injected_dep.exists());
    let metadata = fs::symlink_metadata(&injected_dep).expect("read injected dependency metadata");
    assert!(!metadata.file_type().is_symlink());

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("dependenciesMeta:"));
    assert!(lockfile_content.contains("injected: true"));

    drop(root); // cleanup
}

#[test]
fn frozen_workspace_injected_dependency_should_still_materialize_from_lockfile() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app package dir");
    fs::create_dir_all(&lib_dir).expect("create lib package dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.2.3",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write lib package manifest");
    fs::write(lib_dir.join("index.js"), "module.exports = 'lib';\n").expect("write lib entrypoint");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/lib": "workspace:*"
            },
            "dependenciesMeta": {
                "@repo/lib": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();
    fs::remove_dir_all(app_dir.join("node_modules")).expect("remove node_modules");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install", "--frozen-lockfile"])
        .assert()
        .success();

    let injected_dep = app_dir.join("node_modules/@repo/lib");
    assert!(injected_dep.exists());
    let metadata = fs::symlink_metadata(&injected_dep).expect("read injected dependency metadata");
    assert!(!metadata.file_type().is_symlink());
    assert!(injected_dep.join("package.json").exists());

    drop(root); // cleanup
}

#[test]
fn link_protocol_dependency_should_link_local_package() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("src");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create src dir");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write linked package manifest");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/lib": "link:../src"
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    let linked_dep = app_dir.join("node_modules/@repo/lib");
    assert!(linked_dep.exists());
    assert!(is_symlink_or_junction(&linked_dep).unwrap());

    let lockfile_content =
        fs::read_to_string(app_dir.join("pnpm-lock.yaml")).expect("read app lockfile");
    assert!(lockfile_content.contains("version: link:../src"));

    drop(root); // cleanup
}

#[test]
fn file_protocol_dependency_should_materialize_local_package() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("src");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create src dir");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write file package manifest");
    fs::write(lib_dir.join("index.js"), "module.exports = 'v1';\n").expect("write source file");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/lib": "file:../src"
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    let materialized_dep = app_dir.join("node_modules/@repo/lib");
    assert!(materialized_dep.exists());
    let metadata =
        fs::symlink_metadata(&materialized_dep).expect("read materialized dependency metadata");
    assert!(!metadata.file_type().is_symlink());
    assert_eq!(
        fs::read_to_string(materialized_dep.join("index.js")).expect("read installed file"),
        "module.exports = 'v1';\n"
    );

    let lockfile_content =
        fs::read_to_string(app_dir.join("pnpm-lock.yaml")).expect("read app lockfile");
    assert!(lockfile_content.contains("specifier: file:../src"));
    assert!(lockfile_content.contains("version: file:../src"));

    drop(root); // cleanup
}

#[test]
fn frozen_file_protocol_dependency_should_materialize_local_package_from_lockfile() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("src");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create src dir");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write file package manifest");
    fs::write(lib_dir.join("index.js"), "module.exports = 'v1';\n").expect("write source file");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/lib": "file:../src"
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();
    fs::remove_dir_all(app_dir.join("node_modules")).expect("remove node_modules");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install", "--frozen-lockfile"])
        .assert()
        .success();

    let materialized_dep = app_dir.join("node_modules/@repo/lib");
    assert!(materialized_dep.exists());
    let metadata =
        fs::symlink_metadata(&materialized_dep).expect("read materialized dependency metadata");
    assert!(!metadata.file_type().is_symlink());
    assert!(materialized_dep.join("package.json").exists());

    drop(root); // cleanup
}

#[test]
fn reinstall_should_refresh_file_protocol_dependency_contents() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("src");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create src dir");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write file package manifest");
    fs::write(lib_dir.join("index.js"), "module.exports = 'v1';\n").expect("write source file");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/lib": "file:../src"
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();
    fs::write(lib_dir.join("index.js"), "module.exports = 'v2';\n").expect("update source file");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install"])
        .assert()
        .success();

    let materialized_dep = app_dir.join("node_modules/@repo/lib");
    assert_eq!(
        fs::read_to_string(materialized_dep.join("index.js")).expect("read installed file"),
        "module.exports = 'v2';\n"
    );

    drop(root); // cleanup
}

#[test]
fn npm_alias_dependency_should_install_under_alias_name() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "name": "app",
        "version": "1.0.0",
        "dependencies": {
            "hello-alias": "npm:@pnpm.e2e/hello-world-js-bin@1.0.0"
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet.with_arg("install").assert().success();

    let alias_path = workspace.join("node_modules/hello-alias");
    assert!(alias_path.exists());
    assert!(is_symlink_or_junction(&alias_path).unwrap());

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read lockfile");
    assert!(lockfile_content.contains("specifier: npm:@pnpm.e2e/hello-world-js-bin@1.0.0"));
    assert!(lockfile_content.contains("version: '@pnpm.e2e/hello-world-js-bin@1.0.0'"));

    drop((root, mock_instance)); // cleanup
}
