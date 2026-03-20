pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_fs::symlink_dir;
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

#[cfg(unix)]
fn write_unix_executable(path: &Path, content: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, content).expect("write executable");
    let mut permissions = fs::metadata(path).expect("read metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable permissions");
}

fn write_bin_append_line_script(workspace: &Path, name: &str, line: &str) {
    let bin_dir = workspace.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).expect("create node_modules/.bin");

    #[cfg(windows)]
    {
        fs::write(
            bin_dir.join(format!("{name}.cmd")),
            format!("@echo off\r\necho {line}>> install-order.txt\r\n"),
        )
        .expect("write .cmd file");
    }

    #[cfg(unix)]
    {
        write_unix_executable(
            &bin_dir.join(name),
            &format!("#!/bin/sh\necho {line} >> install-order.txt\n"),
        );
    }
}

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

fn same_physical_file(left: &Path, right: &Path) -> bool {
    same_file::is_same_file(left, right).unwrap_or(false)
}

fn find_virtual_store_file(
    project_dir: &Path,
    package_name: &str,
    file_name: &str,
) -> std::path::PathBuf {
    let suffix = format!("node_modules/{package_name}/{file_name}",).replace('\\', "/");
    project_dir
        .ancestors()
        .map(|dir| dir.join("node_modules/.pnpm"))
        .filter(|virtual_store_dir| virtual_store_dir.is_dir())
        .find_map(|virtual_store_dir| {
            walkdir::WalkDir::new(virtual_store_dir).into_iter().filter_map(Result::ok).find_map(
                |entry| {
                    let entry_path = entry.path();
                    let normalized = entry_path.to_string_lossy().replace('\\', "/");
                    (entry.file_type().is_file() && normalized.ends_with(&suffix))
                        .then(|| entry_path.to_path_buf())
                },
            )
        })
        .unwrap_or_else(|| panic!("find virtual store file for {package_name}/{file_name}"))
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
fn install_should_apply_patched_dependencies_and_refresh_virtual_store_package() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

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

    pacquet_command(&workspace).with_arg("install").assert().success();

    fs::create_dir_all(workspace.join("patches")).expect("create patches dir");
    fs::write(
        workspace.join("patches/hello-world-js-bin.patch"),
        "\
diff --git a/index.js b/index.js
--- a/index.js
+++ b/index.js
@@ -1,2 +1,2 @@
 #!/usr/bin/env node
-console.log('Hello world!')
+console.log('Hello patched world!')
",
    )
    .expect("write patch file");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin": "1.0.0",
            },
            "pnpm": {
                "patchedDependencies": {
                    "@pnpm.e2e/hello-world-js-bin@1.0.0": "patches/hello-world-js-bin.patch"
                }
            }
        })
        .to_string(),
    )
    .expect("rewrite package.json");

    pacquet_command(&workspace).with_arg("install").assert().success();

    let installed_file =
        find_virtual_store_file(&workspace, "@pnpm.e2e/hello-world-js-bin", "index.js");
    let installed_text = fs::read_to_string(installed_file).expect("read patched file");
    assert!(installed_text.contains("Hello patched world!"));

    let lockfile_text =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read pnpm lockfile");
    assert!(lockfile_text.contains("patchedDependencies:"));
    assert!(lockfile_text.contains("'@pnpm.e2e/hello-world-js-bin@1.0.0':"));
    assert!(lockfile_text.contains("patches/hello-world-js-bin.patch"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn install_should_write_modules_manifest_with_pruned_at() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

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

    let modules_manifest = fs::read_to_string(workspace.join("node_modules/.modules.yaml"))
        .expect("read modules yaml");
    assert!(modules_manifest.contains("\"prunedAt\":"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn install_should_execute_project_lifecycle_scripts() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {
                "pnpm:devPreinstall": "devpreinstall-bin",
                "preinstall": "preinstall-bin",
                "install": "install-bin",
                "postinstall": "postinstall-bin",
                "preprepare": "preprepare-bin",
                "prepare": "prepare-bin",
                "postprepare": "postprepare-bin"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    write_bin_append_line_script(&workspace, "devpreinstall-bin", "pnpm:devPreinstall");
    write_bin_append_line_script(&workspace, "preinstall-bin", "preinstall");
    write_bin_append_line_script(&workspace, "install-bin", "install");
    write_bin_append_line_script(&workspace, "postinstall-bin", "postinstall");
    write_bin_append_line_script(&workspace, "preprepare-bin", "preprepare");
    write_bin_append_line_script(&workspace, "prepare-bin", "prepare");
    write_bin_append_line_script(&workspace, "postprepare-bin", "postprepare");

    pacquet.with_arg("install").assert().success();

    let lines =
        fs::read_to_string(workspace.join("install-order.txt")).expect("read install-order");
    assert_eq!(
        lines.lines().collect::<Vec<_>>(),
        vec![
            "pnpm:devPreinstall",
            "preinstall",
            "install",
            "postinstall",
            "preprepare",
            "prepare",
            "postprepare",
        ]
    );

    drop(root); // cleanup
}

#[test]
fn ignore_scripts_should_skip_project_lifecycle_scripts() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {
                "pnpm:devPreinstall": "devpreinstall-bin",
                "preinstall": "preinstall-bin",
                "install": "install-bin",
                "postinstall": "postinstall-bin"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    write_bin_append_line_script(&workspace, "devpreinstall-bin", "pnpm:devPreinstall");
    write_bin_append_line_script(&workspace, "preinstall-bin", "preinstall");
    write_bin_append_line_script(&workspace, "install-bin", "install");
    write_bin_append_line_script(&workspace, "postinstall-bin", "postinstall");

    pacquet.with_args(["install", "--ignore-scripts"]).assert().success();

    assert!(!workspace.join("install-order.txt").exists());

    drop(root); // cleanup
}

#[test]
fn lockfile_only_should_skip_project_lifecycle_scripts() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "scripts": {
                "pnpm:devPreinstall": "devpreinstall-bin",
                "preinstall": "preinstall-bin",
                "install": "install-bin",
                "postinstall": "postinstall-bin"
            }
        })
        .to_string(),
    )
    .expect("write package.json");
    write_bin_append_line_script(&workspace, "devpreinstall-bin", "pnpm:devPreinstall");
    write_bin_append_line_script(&workspace, "preinstall-bin", "preinstall");
    write_bin_append_line_script(&workspace, "install-bin", "install");
    write_bin_append_line_script(&workspace, "postinstall-bin", "postinstall");

    pacquet.with_args(["install", "--lockfile-only"]).assert().success();

    assert!(!workspace.join("install-order.txt").exists());

    drop(root); // cleanup
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
fn link_protocol_dependency_with_directories_bin_should_install_bin_entry() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let pkg_dir = workspace.join("pkg-with-directories-bin");
    fs::create_dir_all(app_dir.join("node_modules")).expect("create app node_modules");
    fs::create_dir_all(pkg_dir.join("bin")).expect("create package bin dir");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "pkg-with-directories-bin": "link:../pkg-with-directories-bin"
            }
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        pkg_dir.join("package.json"),
        serde_json::json!({
            "name": "pkg-with-directories-bin",
            "version": "1.0.0",
            "directories": {
                "bin": "bin"
            }
        })
        .to_string(),
    )
    .expect("write linked package manifest");
    fs::write(pkg_dir.join("bin/pkg-with-directories-bin"), "#!/bin/sh\necho linked\n")
        .expect("write linked bin");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    assert!(installed_bin_path(&app_dir, "pkg-with-directories-bin").exists());

    drop(root);
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
fn prefer_frozen_lockfile_should_succeed_without_current_install_state_when_lockfile_matches() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, cache_dir, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    pacquet_command(&workspace).with_args(["install"]).assert().success();
    assert!(workspace.join("pnpm-lock.yaml").exists());
    fs::remove_dir_all(workspace.join("node_modules")).expect("remove node_modules");

    fs::write(
        workspace.join(".npmrc"),
        format!(
            "registry=http://127.0.0.1:9/\nstore-dir={}\ncache-dir={}\nfetch-timeout=100\nprefer-frozen-lockfile=true\n",
            store_dir.display(),
            cache_dir.display()
        ),
    )
    .expect("rewrite .npmrc");

    pacquet_command(&workspace)
        .with_args(["install", "--prefer-frozen-lockfile"])
        .assert()
        .success();
    assert!(wait_for_path(
        &workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent"),
        Duration::from_secs(2),
    ));

    drop(root); // cleanup
}

#[test]
fn prefer_frozen_lockfile_should_succeed_with_linked_workspace_dependency_from_subproject() {
    let CommandTempCwd { pacquet: _, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, cache_dir, mock_instance, .. } = npmrc_info;

    let project_1_dir = workspace.join("project-1");
    let project_2_dir = workspace.join("project-2");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - project-*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0",
                "project-2": "link:../project-2"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "dependencies": {
                "is-negative": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");

    pacquet_command(&workspace).with_args(["install", "--recursive"]).assert().success();
    fs::remove_dir_all(workspace.join("node_modules")).expect("remove workspace node_modules");
    let _ = fs::remove_dir_all(project_1_dir.join("node_modules"));
    let _ = fs::remove_dir_all(project_2_dir.join("node_modules"));
    fs::write(
        project_1_dir.join(".npmrc"),
        format!(
            "registry=http://127.0.0.1:9/\nstore-dir={}\ncache-dir={}\nfetch-timeout=100\nprefer-frozen-lockfile=true\n",
            store_dir.display(),
            cache_dir.display()
        ),
    )
    .expect("write project-1 npmrc");

    pacquet_command(&workspace)
        .with_args(["-C", project_1_dir.to_str().unwrap(), "install", "--prefer-frozen-lockfile"])
        .assert()
        .success();

    assert!(
        wait_for_path(&project_1_dir.join("node_modules/is-positive"), Duration::from_secs(2),)
    );
    let linked_project = project_1_dir.join("node_modules/project-2");
    assert!(linked_project.exists());
    assert!(is_symlink_or_junction(&linked_project).unwrap());
    assert!(!project_2_dir.join("node_modules/is-negative").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn prefer_frozen_lockfile_should_succeed_with_workspace_protocol_dependency_in_workspace() {
    let CommandTempCwd { pacquet: _, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, cache_dir, mock_instance, .. } = npmrc_info;

    let project_1_dir = workspace.join("project-1");
    let project_2_dir = workspace.join("project-2");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - project-*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0",
                "project-2": "workspace:1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "dependencies": {
                "is-negative": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");

    pacquet_command(&workspace).with_args(["install", "--recursive"]).assert().success();
    fs::remove_dir_all(workspace.join("node_modules")).expect("remove workspace node_modules");
    let _ = fs::remove_dir_all(project_1_dir.join("node_modules"));
    let _ = fs::remove_dir_all(project_2_dir.join("node_modules"));
    fs::write(
        workspace.join(".npmrc"),
        format!(
            "registry=http://127.0.0.1:9/\nstore-dir={}\ncache-dir={}\nfetch-timeout=100\nprefer-frozen-lockfile=true\n",
            store_dir.display(),
            cache_dir.display()
        ),
    )
    .expect("rewrite workspace npmrc");

    pacquet_command(&workspace)
        .with_args(["install", "--recursive", "--prefer-frozen-lockfile"])
        .assert()
        .success();

    assert!(
        wait_for_path(&project_1_dir.join("node_modules/is-positive"), Duration::from_secs(2),)
    );
    let linked_project = project_1_dir.join("node_modules/project-2");
    assert!(linked_project.exists());
    assert!(is_symlink_or_junction(&linked_project).unwrap());
    assert!(
        wait_for_path(&project_2_dir.join("node_modules/is-negative"), Duration::from_secs(2),)
    );

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_protocol_dependency_should_fail_when_workspace_package_is_missing() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let project_1_dir = workspace.join("project-1");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - project-*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "project-2": "workspace:1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");

    let assert = pacquet.with_args(["install", "--recursive"]).assert().failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(stderr.contains("\"project-2@workspace:1.0.0\" is in the dependencies"));
    assert!(stderr.contains("\"project-2\" is present in the workspace"));

    drop(root); // cleanup
}

#[test]
fn workspace_protocol_dependency_should_fail_when_workspace_version_does_not_match() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let project_1_dir = workspace.join("project-1");
    let project_2_dir = workspace.join("project-2");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - project-*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "project-2": "workspace:2.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write project-2 manifest");

    let assert = pacquet.with_args(["install", "--recursive"]).assert().failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(stderr.contains("No matching version found for project-2@workspace:2.0.0"));
    assert!(stderr.contains("inside the"));
    assert!(stderr.contains("workspace. Available versions: 1.0.0"));

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
fn lockfile_only_should_warn_when_current_lockfile_exists() {
    let CommandTempCwd { root, workspace, .. } = CommandTempCwd::init();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write to package.json");
    fs::create_dir_all(workspace.join("node_modules/.pnpm")).expect("create virtual store dir");
    fs::write(
        workspace.join("node_modules/.pnpm/lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .: {}\n",
    )
    .expect("write current lockfile");

    let assert =
        pacquet_command(&workspace).with_args(["install", "--lockfile-only"]).assert().success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stderr.contains(
        "WARN `node_modules` is present. Lockfile only installation will make it out-of-date"
    ));

    drop(root); // cleanup
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
fn reporter_silent_should_suppress_install_output() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    let assert = pacquet.with_args(["install", "--reporter", "silent"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stdout.trim().is_empty());
    assert!(stderr.trim().is_empty());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn reporter_append_only_should_write_static_progress_lines_to_stderr() {
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

    let assert = pacquet.with_args(["install", "--reporter", "append-only"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stdout.contains("Packages: +"));
    assert!(stdout.contains("using pacquet v"));
    assert!(stderr.contains("Progress: resolved "));
    assert!(stderr.contains(", done"));
    assert!(!stderr.contains("\u{1b}["));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn hook_logs_should_be_printed_by_append_only_reporter() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

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
        workspace.join(".pnpmfile.cjs"),
        r#"
module.exports = {
  hooks: {
    readPackage (pkg, ctx) {
      ctx.log("foo");
      return pkg;
    }
  }
}
"#,
    )
    .expect("write pnpmfile");

    let assert = pacquet.with_args(["install", "--reporter", "append-only"]).assert().success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stderr.contains("readPackage: foo"));

    drop(root); // cleanup
}

#[test]
fn hook_logs_should_be_suppressed_by_silent_reporter() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

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
        workspace.join(".pnpmfile.cjs"),
        r#"
module.exports = {
  hooks: {
    readPackage (pkg, ctx) {
      ctx.log("foo");
      return pkg;
    }
  }
}
"#,
    )
    .expect("write pnpmfile");

    let assert = pacquet.with_args(["install", "--reporter", "silent"]).assert().success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stderr.trim().is_empty());

    drop(root); // cleanup
}

#[test]
fn install_should_use_custom_pnpmfile_path() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

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
        workspace.join(".pnpmfile.cjs"),
        r#"
module.exports = {
  hooks: {
    readPackage (pkg, ctx) {
      ctx.log("default");
      return pkg;
    }
  }
}
"#,
    )
    .expect("write default pnpmfile");
    fs::create_dir_all(workspace.join("hooks")).expect("create hooks dir");
    fs::write(
        workspace.join("hooks/custom.cjs"),
        r#"
module.exports = {
  hooks: {
    readPackage (pkg, ctx) {
      ctx.log("custom");
      return pkg;
    }
  }
}
"#,
    )
    .expect("write custom pnpmfile");

    let assert = pacquet
        .with_args(["install", "--reporter", "append-only", "--pnpmfile", "hooks/custom.cjs"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stderr.contains("readPackage: custom"));
    assert!(!stderr.contains("readPackage: default"));

    drop(root); // cleanup
}

#[test]
fn recursive_install_should_prefix_hook_and_progress_output() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - '*'\n  - '*/*'\n")
        .expect("write workspace manifest");
    fs::create_dir_all(workspace.join("packages/app")).expect("create app dir");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "root",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        workspace.join("packages/app/package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app package.json");
    fs::write(
        workspace.join(".pnpmfile.cjs"),
        r#"
module.exports = {
  hooks: {
    readPackage (pkg, ctx) {
      ctx.log("foo");
      return pkg;
    }
  }
}
"#,
    )
    .expect("write pnpmfile");

    let assert = pacquet
        .with_args(["install", "--recursive", "--reporter", "append-only"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stderr.lines().any(|line| line.starts_with(".") && line.contains("readPackage: foo")));
    assert!(
        stderr.lines().any(|line| line.starts_with(".") && line.contains("Progress: resolved"))
    );
    assert!(
        stderr
            .lines()
            .any(|line| line.contains("packages/app") && line.contains("Progress: resolved"))
    );

    drop(root); // cleanup
}

#[test]
fn recursive_install_should_not_print_full_noop_summaries() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - '*'\n  - '*/*'\n")
        .expect("write workspace manifest");
    fs::create_dir_all(workspace.join("packages/app")).expect("create app dir");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "root",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        workspace.join("packages/app/package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app package.json");

    let assert = pacquet
        .with_args(["install", "--recursive", "--reporter", "append-only"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(stdout.trim().is_empty());

    drop(root); // cleanup
}

#[test]
fn recursive_install_should_shorten_long_workspace_prefixes_like_pnpm() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - '*'\n  - '*/*'\n")
        .expect("write workspace manifest");
    let long_dir = workspace.join("loooooooooooooooooooooooooooooooooong").join("pkg-3");
    fs::create_dir_all(&long_dir).expect("create long app dir");
    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "root",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        long_dir.join("package.json"),
        serde_json::json!({
            "name": "pkg-3",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write long package.json");

    let assert = pacquet
        .with_args(["install", "--recursive", "--reporter", "append-only"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with(".../pkg-3") && line.contains("Progress: resolved"))
    );

    drop(root); // cleanup
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
    assert!(
        workspace.join("node_modules/.pnpm/@pnpm.e2e+hello-world-js-bin-parent@1.0.0").exists()
    );
    assert!(!project_dir.join("node_modules/.pnpm").exists());

    let lockfile_content = fs::read_to_string(root_lockfile).expect("read lockfile");
    assert!(lockfile_content.contains("importers:"));
    assert!(lockfile_content.contains("packages/app:"));

    let workspace_state_path = workspace.join("node_modules/.pnpm-workspace-state-v1.json");
    assert!(workspace_state_path.exists());
    let workspace_state = serde_json::from_str::<serde_json::Value>(
        &fs::read_to_string(workspace_state_path).expect("read workspace state"),
    )
    .expect("parse workspace state");
    assert_eq!(
        workspace_state.get("filteredInstall").and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        workspace_state
            .get("settings")
            .and_then(|settings| settings.get("nodeLinker"))
            .and_then(serde_json::Value::as_str),
        Some("isolated")
    );
    let workspace_state_text =
        fs::read_to_string(workspace.join("node_modules/.pnpm-workspace-state-v1.json"))
            .expect("read workspace state text");
    assert!(workspace_state_text.contains("\"name\": \"app\""));

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
fn workspace_filtered_frozen_install_should_not_prune_other_importer_packages_from_virtual_store() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

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

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--recursive"])
        .assert()
        .success();

    let lib_virtual_store_package =
        workspace.join("node_modules/.pnpm/@pnpm.e2e+hello-world-js-bin@1.0.0");
    assert!(lib_virtual_store_package.exists());
    assert!(lib_dir.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists());

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args([
            "install",
            "--recursive",
            "--filter",
            "@repo/app",
            "--frozen-lockfile",
            "--force",
        ])
        .assert()
        .success();

    assert!(lib_virtual_store_package.exists());
    assert!(lib_dir.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists());
    assert!(app_dir.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());
    let current_lockfile = fs::read_to_string(workspace.join("node_modules/.pnpm/lock.yaml"))
        .expect("read current lockfile");
    assert!(current_lockfile.contains("packages/app:"));
    assert!(current_lockfile.contains("packages/lib:"));
    assert!(current_lockfile.contains("@pnpm.e2e/hello-world-js-bin@1.0.0"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_filtered_install_should_not_prune_other_importer_packages_from_virtual_store() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

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
                "is-positive": "1.0.0"
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

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--recursive"])
        .assert()
        .success();

    let lib_virtual_store_package =
        workspace.join("node_modules/.pnpm/@pnpm.e2e+hello-world-js-bin-parent@1.0.0");
    assert!(lib_virtual_store_package.exists());
    assert!(lib_dir.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());

    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/app",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "2.0.0"
            }
        })
        .to_string(),
    )
    .expect("update app package.json");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--recursive", "--filter", "@repo/app"])
        .assert()
        .success();

    assert!(workspace.join("node_modules/.pnpm/is-positive@2.0.0").exists());
    assert!(lib_virtual_store_package.exists());
    assert!(lib_dir.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());
    let current_lockfile = fs::read_to_string(workspace.join("node_modules/.pnpm/lock.yaml"))
        .expect("read current lockfile");
    assert!(current_lockfile.contains("packages/app:"));
    assert!(current_lockfile.contains("packages/lib:"));
    assert!(current_lockfile.contains("is-positive@2.0.0"));
    assert!(current_lockfile.contains("@pnpm.e2e/hello-world-js-bin-parent@1.0.0"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn current_lockfile_should_only_contain_installed_dependencies_when_adding_new_workspace_importer()
{
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

    let project_1_dir = workspace.join("project-1");
    let project_2_dir = workspace.join("project-2");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - project-*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "dependencies": {
                "is-negative": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["-C", project_1_dir.to_str().unwrap(), "install", "--lockfile-only"])
        .assert()
        .success();
    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["-C", project_2_dir.to_str().unwrap(), "install"])
        .assert()
        .success();

    let current_lockfile = fs::read_to_string(workspace.join("node_modules/.pnpm/lock.yaml"))
        .expect("read current lockfile");
    assert!(current_lockfile.contains("project-2:"));
    assert!(current_lockfile.contains("is-negative@1.0.0:"));
    assert!(!current_lockfile.contains("project-1:"));
    assert!(!current_lockfile.contains("is-positive@1.0.0:"));
    assert!(!project_1_dir.join("node_modules/is-positive").exists());
    assert!(project_2_dir.join("node_modules/is-negative").exists());

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
fn workspace_protocol_relative_path_dependency_should_write_linked_importer_entry() {
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
                "@repo/lib": "workspace:../lib"
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("specifier: workspace:../lib"));
    assert!(lockfile_content.contains("version: link:../lib"));

    drop(root); // cleanup
}

#[test]
fn workspace_protocol_dependency_should_link_publish_directory_when_link_directory_is_true() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let pacquet_bin = pacquet.get_program().to_os_string();

    let project_1_dir = workspace.join("project-1");
    let project_1_dist_dir = project_1_dir.join("dist");
    let project_2_dir = workspace.join("project-2");
    fs::create_dir_all(&project_1_dist_dir).expect("create project-1 dist dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - project-*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "publishConfig": {
                "directory": "dist",
                "linkDirectory": true
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_1_dist_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1-dist",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write dist manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "dependencies": {
                "project-1": "workspace:*"
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["-C", workspace.to_str().unwrap(), "install", "-r"])
        .assert()
        .success();

    let linked_manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project_2_dir.join("node_modules/project-1/package.json"))
            .expect("read linked manifest"),
    )
    .expect("parse linked manifest");
    assert_eq!(
        linked_manifest.get("name").and_then(serde_json::Value::as_str),
        Some("project-1-dist")
    );

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("project-1:"));
    assert!(lockfile_content.contains("publishDirectory: dist"));

    fs::remove_dir_all(workspace.join("node_modules")).expect("remove workspace node_modules");
    let _ = fs::remove_dir_all(project_1_dir.join("node_modules"));
    let _ = fs::remove_dir_all(project_2_dir.join("node_modules"));

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["-C", workspace.to_str().unwrap(), "install", "-r", "--frozen-lockfile"])
        .assert()
        .success();

    let linked_manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project_2_dir.join("node_modules/project-1/package.json"))
            .expect("read linked manifest after frozen install"),
    )
    .expect("parse linked manifest after frozen install");
    assert_eq!(
        linked_manifest.get("name").and_then(serde_json::Value::as_str),
        Some("project-1-dist")
    );

    drop(root); // cleanup
}

#[test]
fn workspace_protocol_dependency_should_not_link_publish_directory_when_link_directory_is_false() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let pacquet_bin = pacquet.get_program().to_os_string();

    let project_1_dir = workspace.join("project-1");
    let project_1_dist_dir = project_1_dir.join("dist");
    let project_2_dir = workspace.join("project-2");
    fs::create_dir_all(&project_1_dist_dir).expect("create project-1 dist dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - project-*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "publishConfig": {
                "directory": "dist",
                "linkDirectory": false
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_1_dist_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1-dist",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write dist manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "dependencies": {
                "project-1": "workspace:*"
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["-C", workspace.to_str().unwrap(), "install", "-r"])
        .assert()
        .success();

    let linked_manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project_2_dir.join("node_modules/project-1/package.json"))
            .expect("read linked manifest"),
    )
    .expect("parse linked manifest");
    assert_eq!(linked_manifest.get("name").and_then(serde_json::Value::as_str), Some("project-1"));

    drop(root); // cleanup
}

#[test]
fn linked_workspace_bin_created_by_prepare_should_be_available_after_recursive_install() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let pacquet_bin = pacquet.get_program().to_os_string();

    let project_1_dir = workspace.join("project-1");
    let project_2_dir = workspace.join("project-2");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - project-*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "scripts": {
                "prepare": "bin"
            },
            "dependencies": {
                "project-2": "link:../project-2"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "bin": {
                "bin": "bin.js"
            },
            "scripts": {
                "prepare": "node prepare.js"
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");
    fs::write(project_2_dir.join("prepare.js"), "require('fs').renameSync('__bin.js', 'bin.js')\n")
        .expect("write prepare script");
    fs::write(
        project_2_dir.join("__bin.js"),
        "#!/usr/bin/env node\nrequire('fs').writeFileSync('created-by-prepare', '', 'utf8')\n",
    )
    .expect("write deferred bin");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["-C", workspace.to_str().unwrap(), "install", "-r"])
        .assert()
        .success();

    assert!(project_1_dir.join("created-by-prepare").exists());

    fs::remove_dir_all(workspace.join("node_modules")).expect("remove workspace node_modules");
    let _ = fs::remove_dir_all(project_1_dir.join("node_modules"));
    let _ = fs::remove_dir_all(project_2_dir.join("node_modules"));
    fs::rename(project_2_dir.join("bin.js"), project_2_dir.join("__bin.js"))
        .expect("restore deferred bin");
    let _ = fs::remove_file(project_1_dir.join("created-by-prepare"));

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["-C", workspace.to_str().unwrap(), "install", "-r", "--frozen-lockfile"])
        .assert()
        .success();

    assert!(project_1_dir.join("created-by-prepare").exists());

    drop(root); // cleanup
}

#[test]
fn recursive_install_should_run_workspace_lifecycle_scripts_in_topological_order_for_semver_workspace_links()
 {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let pacquet_bin = pacquet.get_program().to_os_string();

    let dep_dir = workspace.join("packages/dep");
    let app_dir = workspace.join("packages/app");
    fs::create_dir_all(&dep_dir).expect("create dep dir");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(workspace.join(".npmrc"), "link-workspace-packages=true\n")
        .expect("write workspace npmrc");

    fs::write(
        dep_dir.join("package.json"),
        serde_json::json!({
            "name": "dep",
            "version": "1.0.0",
            "scripts": {
                "install": "node install.js",
                "postinstall": "node postinstall.js",
                "prepare": "node prepare.js"
            }
        })
        .to_string(),
    )
    .expect("write dep manifest");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "dep": "1.0.0"
            },
            "scripts": {
                "install": "node install.js",
                "postinstall": "node postinstall.js",
                "prepare": "node prepare.js"
            }
        })
        .to_string(),
    )
    .expect("write app manifest");

    for (project_dir, project_name) in [(&dep_dir, "dep"), (&app_dir, "app")] {
        for script_name in ["install", "postinstall", "prepare"] {
            fs::write(
                project_dir.join(format!("{script_name}.js")),
                format!(
                    "require('fs').appendFileSync({:?}, {:?} + '\\n', 'utf8')\n",
                    workspace.join("install-order.txt"),
                    format!("{project_name}-{script_name}")
                ),
            )
            .expect("write lifecycle script");
        }
    }

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["-C", workspace.to_str().unwrap(), "install", "-r"])
        .assert()
        .success();

    let install_order =
        fs::read_to_string(workspace.join("install-order.txt")).expect("read install-order");
    assert_eq!(
        install_order.lines().collect::<Vec<_>>(),
        vec![
            "dep-install",
            "dep-postinstall",
            "dep-prepare",
            "app-install",
            "app-postinstall",
            "app-prepare",
        ]
    );

    fs::remove_file(workspace.join("install-order.txt")).expect("remove install-order");
    fs::remove_dir_all(workspace.join("node_modules")).expect("remove workspace node_modules");
    let _ = fs::remove_dir_all(dep_dir.join("node_modules"));
    let _ = fs::remove_dir_all(app_dir.join("node_modules"));

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["-C", workspace.to_str().unwrap(), "install", "-r", "--frozen-lockfile"])
        .assert()
        .success();

    let frozen_install_order =
        fs::read_to_string(workspace.join("install-order.txt")).expect("read frozen install-order");
    assert_eq!(
        frozen_install_order.lines().collect::<Vec<_>>(),
        vec![
            "dep-install",
            "dep-postinstall",
            "dep-prepare",
            "app-install",
            "app-postinstall",
            "app-prepare",
        ]
    );

    drop(root); // cleanup
}

#[test]
fn semver_workspace_dependency_should_not_link_local_package_by_default() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let app_dir = workspace.join("packages/app");
    let local_is_positive_dir = workspace.join("packages/is-positive");
    fs::create_dir_all(&app_dir).expect("create app package dir");
    fs::create_dir_all(&local_is_positive_dir).expect("create local is-positive dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        local_is_positive_dir.join("package.json"),
        serde_json::json!({
            "name": "is-positive",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write local is-positive manifest");
    fs::write(local_is_positive_dir.join("index.js"), "module.exports = 'workspace';\n")
        .expect("write local is-positive entrypoint");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    let installed_dep = app_dir.join("node_modules/is-positive");
    assert!(installed_dep.exists());
    assert_ne!(
        installed_dep.canonicalize().expect("canonicalize installed dep"),
        local_is_positive_dir.canonicalize().expect("canonicalize workspace package"),
    );

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("is-positive:"));
    assert!(lockfile_content.contains("version: 1.0.0"));
    assert!(!lockfile_content.contains("version: link:../is-positive"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn semver_workspace_dependency_should_link_local_package_when_link_workspace_packages_is_true() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

    let app_dir = workspace.join("packages/app");
    let local_is_positive_dir = workspace.join("packages/is-positive");
    fs::create_dir_all(&app_dir).expect("create app package dir");
    fs::create_dir_all(&local_is_positive_dir).expect("create local is-positive dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(workspace.join(".npmrc"), "link-workspace-packages=true\n")
        .expect("write workspace npmrc");
    fs::write(app_dir.join(".npmrc"), "link-workspace-packages=true\n").expect("write app npmrc");
    fs::write(
        local_is_positive_dir.join("package.json"),
        serde_json::json!({
            "name": "is-positive",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write local is-positive manifest");
    fs::write(local_is_positive_dir.join("index.js"), "module.exports = 'workspace';\n")
        .expect("write local is-positive entrypoint");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&app_dir)
        .with_arg("install")
        .assert()
        .success();

    let installed_dep = app_dir.join("node_modules/is-positive");
    assert!(installed_dep.exists());
    assert_eq!(
        installed_dep.canonicalize().expect("canonicalize installed dep"),
        local_is_positive_dir.canonicalize().expect("canonicalize workspace package"),
    );

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("version: link:../is-positive"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn install_should_relink_workspace_packages_when_link_workspace_packages_is_enabled_after_registry_install()
 {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

    let project_dir = workspace.join("project");
    let local_is_positive_dir = workspace.join("is-positive");
    let local_is_negative_dir = workspace.join("is-negative");
    fs::create_dir_all(&project_dir).expect("create project dir");
    fs::create_dir_all(&local_is_positive_dir).expect("create local is-positive dir");
    fs::create_dir_all(&local_is_negative_dir).expect("create local is-negative dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - '*'\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(workspace.join(".npmrc"), "link-workspace-packages=false\n")
        .expect("write workspace npmrc");
    fs::write(
        local_is_positive_dir.join("package.json"),
        serde_json::json!({
            "name": "is-positive",
            "version": "3.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write local is-positive manifest");
    fs::write(local_is_positive_dir.join("index.js"), "module.exports = 'workspace';\n")
        .expect("write local is-positive entrypoint");
    fs::write(
        local_is_negative_dir.join("package.json"),
        serde_json::json!({
            "name": "is-negative",
            "version": "3.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write local is-negative manifest");
    fs::write(local_is_negative_dir.join("index.js"), "module.exports = 'workspace';\n")
        .expect("write local is-negative entrypoint");
    fs::write(
        project_dir.join("package.json"),
        serde_json::json!({
            "name": "project",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "2.0.0",
                "negative": "npm:is-negative@1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project manifest");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_arg("install")
        .assert()
        .success();

    let first_lockfile =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read first lockfile");
    assert!(first_lockfile.contains("is-positive:"));
    assert!(first_lockfile.contains("version: 2.0.0"));
    assert!(first_lockfile.contains("negative:"));
    assert!(first_lockfile.contains("version: is-negative@1.0.0"));

    fs::write(workspace.join(".npmrc"), "link-workspace-packages=true\n")
        .expect("rewrite workspace npmrc");
    fs::write(
        local_is_positive_dir.join("package.json"),
        serde_json::json!({
            "name": "is-positive",
            "version": "2.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("rewrite local is-positive manifest");
    fs::write(
        local_is_negative_dir.join("package.json"),
        serde_json::json!({
            "name": "is-negative",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("rewrite local is-negative manifest");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_arg("install")
        .assert()
        .success();

    let second_lockfile =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read second lockfile");
    assert!(second_lockfile.contains("version: link:../is-positive"));
    assert!(second_lockfile.contains("version: link:../is-negative"));

    let installed_positive = project_dir.join("node_modules/is-positive");
    let installed_negative = project_dir.join("node_modules/negative");
    assert_eq!(
        installed_positive.canonicalize().expect("canonicalize installed positive"),
        local_is_positive_dir.canonicalize().expect("canonicalize workspace is-positive"),
    );
    assert_eq!(
        installed_negative.canonicalize().expect("canonicalize installed negative"),
        local_is_negative_dir.canonicalize().expect("canonicalize workspace is-negative"),
    );

    drop((root, mock_instance)); // cleanup
}

#[test]
fn transitive_workspace_dependency_should_link_local_package_when_link_workspace_packages_is_deep()
{
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

    let app_dir = workspace.join("packages/app");
    let parent_dir = workspace.join("packages/parent");
    let local_dep_dir = workspace.join("packages/is-positive");
    fs::create_dir_all(&app_dir).expect("create app package dir");
    fs::create_dir_all(&parent_dir).expect("create parent package dir");
    fs::create_dir_all(&local_dep_dir).expect("create local dependency dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(workspace.join(".npmrc"), "link-workspace-packages=deep\n")
        .expect("write workspace npmrc");
    fs::write(
        parent_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/parent",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write parent manifest");
    fs::write(
        local_dep_dir.join("package.json"),
        serde_json::json!({
            "name": "is-positive",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write local dependency manifest");
    fs::write(local_dep_dir.join("index.js"), "module.exports = 'workspace';\n")
        .expect("write local dependency entrypoint");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/parent": "workspace:*"
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--recursive"])
        .assert()
        .success();

    let wanted_lockfile =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(wanted_lockfile.contains("packages/parent:"));
    assert!(wanted_lockfile.contains("version: link:../parent"));
    assert!(wanted_lockfile.contains("is-positive:"));
    assert!(wanted_lockfile.contains("version: link:../is-positive"));

    fs::remove_dir_all(workspace.join("node_modules")).expect("remove node_modules");
    let _ = fs::remove_dir_all(app_dir.join("node_modules"));
    let _ = fs::remove_dir_all(parent_dir.join("node_modules"));
    let _ = fs::remove_dir_all(local_dep_dir.join("node_modules"));

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--recursive", "--frozen-lockfile"])
        .assert()
        .success();

    let linked_dep = app_dir.join("node_modules/@repo/parent/node_modules/is-positive");
    assert!(linked_dep.exists());
    assert_eq!(
        linked_dep.canonicalize().expect("canonicalize installed dep"),
        local_dep_dir.canonicalize().expect("canonicalize workspace package"),
    );

    drop((root, mock_instance)); // cleanup
}

#[test]
fn transitive_workspace_peer_dependency_should_link_local_package_when_link_workspace_packages_is_deep()
 {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let pacquet_bin = pacquet.get_program().to_os_string();

    let app_dir = workspace.join("packages/app");
    let parent_dir = workspace.join("packages/parent");
    let child_dir = workspace.join("packages/child");
    let peer_a_dir = workspace.join("packages/peer-a");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&parent_dir).expect("create parent dir");
    fs::create_dir_all(&child_dir).expect("create child dir");
    fs::create_dir_all(&peer_a_dir).expect("create peer-a dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        workspace.join(".npmrc"),
        "inject-workspace-packages=true\nlink-workspace-packages=deep\nauto-install-peers=false\ndedupe-injected-deps=false\nstrict-peer-dependencies=false\n",
    )
    .expect("write workspace npmrc");
    fs::write(
        child_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/child",
            "version": "1.0.0",
            "peerDependencies": {
                "@repo/peer-a": "^1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write child manifest");
    fs::write(
        parent_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/parent",
            "version": "1.0.0",
            "dependencies": {
                "@repo/child": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write parent manifest");
    fs::write(
        peer_a_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/peer-a",
            "version": "1.0.1",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write peer-a manifest");
    fs::write(peer_a_dir.join("index.js"), "module.exports = 'peer-a';\n")
        .expect("write peer-a entrypoint");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/parent": "workspace:*"
            }
        })
        .to_string(),
    )
    .expect("write app manifest");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--recursive"])
        .assert()
        .success();

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("@repo/parent@file:packages/parent"));
    assert!(lockfile_content.contains("file:packages/child(@repo/peer-a@packages/peer-a)"));
    assert!(lockfile_content.contains("link:packages/peer-a"));

    fs::remove_dir_all(workspace.join("node_modules")).expect("remove root node_modules");
    let _ = fs::remove_dir_all(app_dir.join("node_modules"));
    let _ = fs::remove_dir_all(parent_dir.join("node_modules"));
    let _ = fs::remove_dir_all(child_dir.join("node_modules"));
    let _ = fs::remove_dir_all(peer_a_dir.join("node_modules"));

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--recursive", "--frozen-lockfile"])
        .assert()
        .success();

    let installed_peer = app_dir
        .join("node_modules/@repo/parent/node_modules/@repo/child/node_modules/@repo/peer-a");
    assert!(installed_peer.exists());
    assert_eq!(
        installed_peer.canonicalize().expect("canonicalize installed peer-a"),
        peer_a_dir.canonicalize().expect("canonicalize workspace peer-a package"),
    );

    drop(root); // cleanup
}

#[test]
fn transitive_workspace_dependency_should_link_local_package_when_overridden_to_workspace_protocol()
{
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

    let app_dir = workspace.join("packages/app");
    let parent_dir = workspace.join("packages/parent");
    let local_dep_dir = workspace.join("packages/is-positive");
    fs::create_dir_all(&app_dir).expect("create app package dir");
    fs::create_dir_all(&parent_dir).expect("create parent package dir");
    fs::create_dir_all(&local_dep_dir).expect("create local dependency dir");
    fs::write(
        workspace.join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*\noverrides:\n  is-positive: workspace:*\n",
    )
    .expect("write pnpm-workspace.yaml");
    fs::write(
        parent_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/parent",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write parent manifest");
    fs::write(
        local_dep_dir.join("package.json"),
        serde_json::json!({
            "name": "is-positive",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write local dependency manifest");
    fs::write(local_dep_dir.join("index.js"), "module.exports = 'workspace';\n")
        .expect("write local dependency entrypoint");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "@repo/parent": "workspace:*"
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--recursive"])
        .assert()
        .success();

    let wanted_lockfile =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(wanted_lockfile.contains("overrides:"));
    assert!(wanted_lockfile.contains("is-positive: workspace:*"));
    assert!(wanted_lockfile.contains("packages/parent:"));
    assert!(wanted_lockfile.contains("version: link:../is-positive"));

    fs::remove_dir_all(workspace.join("node_modules")).expect("remove node_modules");
    let _ = fs::remove_dir_all(app_dir.join("node_modules"));
    let _ = fs::remove_dir_all(parent_dir.join("node_modules"));
    let _ = fs::remove_dir_all(local_dep_dir.join("node_modules"));

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--recursive", "--frozen-lockfile"])
        .assert()
        .success();

    let linked_dep = app_dir.join("node_modules/@repo/parent/node_modules/is-positive");
    assert!(linked_dep.exists());
    assert_eq!(
        linked_dep.canonicalize().expect("canonicalize installed dep"),
        local_dep_dir.canonicalize().expect("canonicalize workspace package"),
    );

    drop((root, mock_instance)); // cleanup
}

#[test]
fn transitive_registry_dependency_should_link_workspace_override_when_subdependency_uses_workspace_protocol()
 {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

    let project_dir = workspace.join("project");
    let local_dep_dir = workspace.join("@pnpm.e2e/dep-of-pkg-with-1-dep");
    fs::create_dir_all(&project_dir).expect("create project dir");
    fs::create_dir_all(&local_dep_dir).expect("create local dependency dir");
    fs::write(
        workspace.join("pnpm-workspace.yaml"),
        "packages:\n  - project\n  - '@pnpm.e2e/*'\noverrides:\n  '@pnpm.e2e/dep-of-pkg-with-1-dep': workspace:*\n",
    )
    .expect("write pnpm-workspace.yaml");
    fs::write(
        project_dir.join("package.json"),
        serde_json::json!({
            "name": "project",
            "version": "1.0.0",
            "dependencies": {
                "@pnpm.e2e/pkg-with-1-dep": "100.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project manifest");
    fs::write(
        local_dep_dir.join("package.json"),
        serde_json::json!({
            "name": "@pnpm.e2e/dep-of-pkg-with-1-dep",
            "version": "100.1.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write local dependency manifest");
    fs::write(local_dep_dir.join("index.js"), "module.exports = 'workspace override';\n")
        .expect("write local dependency entrypoint");

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--recursive"])
        .assert()
        .success();

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("link:@pnpm.e2e/dep-of-pkg-with-1-dep"));

    fs::remove_dir_all(workspace.join("node_modules")).expect("remove node_modules");
    let _ = fs::remove_dir_all(project_dir.join("node_modules"));
    let _ = fs::remove_dir_all(local_dep_dir.join("node_modules"));

    std::process::Command::new(&pacquet_bin)
        .with_current_dir(&workspace)
        .with_args(["install", "--recursive", "--frozen-lockfile"])
        .assert()
        .success();

    drop((root, mock_instance)); // cleanup
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
    assert!(lockfile_content.contains("version: file:packages/lib"));
    assert!(lockfile_content.contains("'@repo/lib@file:packages/lib':"));
    assert!(lockfile_content.contains("directory: packages/lib"));

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

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("version: file:packages/lib"));

    drop(root); // cleanup
}

#[test]
fn workspace_injected_dependency_should_write_peer_suffixed_local_snapshot() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

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
            "main": "index.js",
            "peerDependencies": {
                "is-number": "^7.0.0"
            }
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
            "devDependencies": {
                "is-number": "7.0.0"
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
    assert!(injected_dep.join("node_modules/is-number").exists());

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("version: file:packages/lib(is-number@7.0.0)"));
    assert!(lockfile_content.contains("'@repo/lib@file:packages/lib(is-number@7.0.0)':"));
    assert!(lockfile_content.contains("peerDependencies:"));
    assert!(lockfile_content.contains("is-number: ^7.0.0"));

    drop(mock_instance);
    drop(root); // cleanup
}

#[test]
fn workspace_injected_dependency_should_dedupe_to_link_when_target_importer_matches() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let project_1_dir = workspace.join("packages/project-1");
    let project_2_dir = workspace.join("packages/project-2");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "is-number": "7.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "dependencies": {
                "project-1": "workspace:*"
            },
            "dependenciesMeta": {
                "project-1": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");

    pacquet.with_args(["-C", workspace.to_str().unwrap(), "install", "-r"]).assert().success();

    let deduped_dep = project_2_dir.join("node_modules/project-1");
    assert!(deduped_dep.exists());
    assert!(is_symlink_or_junction(&deduped_dep).unwrap());

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("packages/project-2:"));
    assert!(lockfile_content.contains("version: link:../project-1"));
    assert!(!lockfile_content.contains("'project-1@file:packages/project-1':"));

    drop(mock_instance);
    drop(root); // cleanup
}

#[test]
fn frozen_workspace_injected_dependency_should_keep_deduped_link_from_lockfile() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let project_1_dir = workspace.join("packages/project-1");
    let project_2_dir = workspace.join("packages/project-2");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "is-number": "7.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "dependencies": {
                "project-1": "workspace:*"
            },
            "dependenciesMeta": {
                "project-1": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");

    pacquet.with_args(["-C", workspace.to_str().unwrap(), "install", "-r"]).assert().success();
    fs::remove_dir_all(project_2_dir.join("node_modules")).expect("remove project-2 node_modules");

    pacquet_command(&workspace)
        .with_args(["-C", workspace.to_str().unwrap(), "install", "-r", "--frozen-lockfile"])
        .assert()
        .success();

    let deduped_dep = project_2_dir.join("node_modules/project-1");
    assert!(deduped_dep.exists());
    assert!(is_symlink_or_junction(&deduped_dep).unwrap());

    drop(mock_instance);
    drop(root); // cleanup
}

#[test]
fn workspace_injected_dependency_should_snapshot_nested_workspace_dependency() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let project_1_dir = workspace.join("packages/project-1");
    let project_2_dir = workspace.join("packages/project-2");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(project_1_dir.join("index.js"), "module.exports = 'project-1';\n")
        .expect("write project-1 entrypoint");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "main": "index.js",
            "dependencies": {
                "project-1": "workspace:*"
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");
    fs::write(project_2_dir.join("index.js"), "module.exports = 'project-2';\n")
        .expect("write project-2 entrypoint");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "project-2": "workspace:*"
            },
            "dependenciesMeta": {
                "project-2": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write app manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    let injected_dep = app_dir.join("node_modules/project-2");
    assert!(injected_dep.exists());
    assert!(injected_dep.join("node_modules/project-1").exists());

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("project-2@file:packages/project-2:"));
    assert!(lockfile_content.contains("project-1: file:packages/project-1"));
    assert!(lockfile_content.contains("project-1@file:packages/project-1:"));

    drop(root); // cleanup
}

#[test]
fn workspace_injected_dependency_should_snapshot_nested_workspace_dependency_with_consumer_peer_context()
 {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let app_dir = workspace.join("packages/app");
    let project_1_dir = workspace.join("packages/project-1");
    let project_2_dir = workspace.join("packages/project-2");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "main": "index.js",
            "peerDependencies": {
                "is-number": "^7.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(project_1_dir.join("index.js"), "module.exports = 'project-1';\n")
        .expect("write project-1 entrypoint");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "main": "index.js",
            "dependencies": {
                "project-1": "workspace:*"
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");
    fs::write(project_2_dir.join("index.js"), "module.exports = 'project-2';\n")
        .expect("write project-2 entrypoint");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "project-2": "workspace:*"
            },
            "devDependencies": {
                "is-number": "7.0.0"
            },
            "dependenciesMeta": {
                "project-2": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write app manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    let injected_dep = app_dir.join("node_modules/project-2");
    assert!(injected_dep.exists());
    assert!(injected_dep.join("node_modules/project-1").exists());
    assert!(injected_dep.join("node_modules/project-1/node_modules/is-number").exists());

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("version: file:packages/project-2(is-number@7.0.0)"));
    assert!(lockfile_content.contains("project-2@file:packages/project-2(is-number@7.0.0):"));
    assert!(lockfile_content.contains("project-1: file:packages/project-1(is-number@7.0.0)"));
    assert!(lockfile_content.contains("project-1@file:packages/project-1(is-number@7.0.0):"));

    drop(mock_instance);
    drop(root); // cleanup
}

#[test]
fn workspace_injected_dependency_should_not_modify_source_manifest() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let project_1_dir = workspace.join("packages/project-1");
    let project_2_dir = workspace.join("packages/project-2");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");

    let project_1_manifest = serde_json::json!({
        "name": "project-1",
        "version": "1.0.0",
        "dependencies": {
            "is-positive": "1.0.0"
        },
        "peerDependencies": {
            "is-positive": ">=1.0.0"
        }
    });
    fs::write(project_1_dir.join("package.json"), project_1_manifest.to_string())
        .expect("write project-1 manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "dependencies": {
                "project-1": "workspace:*"
            },
            "devDependencies": {
                "is-positive": "1.0.0"
            },
            "dependenciesMeta": {
                "project-1": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");

    let before = fs::read_to_string(project_1_dir.join("package.json"))
        .expect("read source project manifest before install");

    pacquet.with_args(["-C", workspace.to_str().unwrap(), "install", "-r"]).assert().success();

    let after = fs::read_to_string(project_1_dir.join("package.json"))
        .expect("read source project manifest after install");
    assert_eq!(after, before);

    let parsed_after: serde_json::Value =
        serde_json::from_str(&after).expect("parse source project manifest");
    assert_eq!(parsed_after, project_1_manifest);

    drop(mock_instance);
    drop(root); // cleanup
}

#[test]
fn peer_dependency_of_injected_workspace_project_should_resolve_to_workspace_link() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let project_1_dir = workspace.join("packages/project-1");
    let project_2_dir = workspace.join("packages/project-2");
    let project_3_dir = workspace.join("packages/project-3");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::create_dir_all(&project_3_dir).expect("create project-3 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(workspace.join(".npmrc"), "node-linker=hoisted\ndedupe-injected-deps=false\n")
        .expect("write workspace npmrc");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "devDependencies": {
                "project-1": "workspace:*"
            },
            "peerDependencies": {
                "project-1": "workspace:^1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");
    fs::write(
        project_3_dir.join("package.json"),
        serde_json::json!({
            "name": "project-3",
            "version": "1.0.0",
            "dependencies": {
                "project-1": "workspace:*",
                "project-2": "workspace:*"
            },
            "dependenciesMeta": {
                "project-2": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write project-3 manifest");

    pacquet.with_args(["-C", workspace.to_str().unwrap(), "install", "-r"]).assert().success();

    let injected_dep = project_3_dir.join("node_modules/project-2");
    assert!(injected_dep.exists());
    assert!(injected_dep.join("node_modules/project-1").exists());
    assert!(is_symlink_or_junction(&injected_dep.join("node_modules/project-1")).unwrap());

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(
        lockfile_content
            .contains("project-2@file:packages/project-2(project-1@packages/project-1):")
    );
    assert!(lockfile_content.contains("project-1: link:packages/project-1"));

    drop(root); // cleanup
}

#[test]
fn workspace_injected_dependency_should_only_dedupe_matching_transitive_peer_context() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let store_dir = root.path().join("pacquet-store");
    let cache_dir = root.path().join("pacquet-cache");
    fs::write(
        workspace.join(".npmrc"),
        format!(
            "registry=https://registry.npmjs.org/\nstore-dir={}\ncache-dir={}\nfetch-timeout=1000\n",
            store_dir.display(),
            cache_dir.display()
        ),
    )
    .expect("write .npmrc");

    let project_1_dir = workspace.join("packages/project-1");
    let project_2_dir = workspace.join("packages/project-2");
    let project_3_dir = workspace.join("packages/project-3");
    let project_4_dir = workspace.join("packages/project-4");
    let project_5_dir = workspace.join("packages/project-5");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::create_dir_all(&project_3_dir).expect("create project-3 dir");
    fs::create_dir_all(&project_4_dir).expect("create project-4 dir");
    fs::create_dir_all(&project_5_dir).expect("create project-5 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "main": "index.js",
            "peerDependencies": {
                "is-positive": ">=1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(project_1_dir.join("index.js"), "module.exports = 'project-1';\n")
        .expect("write project-1 entrypoint");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "main": "index.js",
            "dependencies": {
                "project-1": "workspace:*"
            },
            "devDependencies": {
                "is-positive": "1.0.0"
            },
            "dependenciesMeta": {
                "project-1": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");
    fs::write(project_2_dir.join("index.js"), "module.exports = 'project-2';\n")
        .expect("write project-2 entrypoint");
    fs::write(
        project_3_dir.join("package.json"),
        serde_json::json!({
            "name": "project-3",
            "version": "1.0.0",
            "dependencies": {
                "project-2": "workspace:*"
            },
            "devDependencies": {
                "is-positive": "2.0.0"
            },
            "dependenciesMeta": {
                "project-2": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write project-3 manifest");
    fs::write(
        project_4_dir.join("package.json"),
        serde_json::json!({
            "name": "project-4",
            "version": "1.0.0",
            "dependencies": {
                "project-2": "workspace:*"
            },
            "devDependencies": {
                "is-positive": "1.0.0"
            },
            "dependenciesMeta": {
                "project-2": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write project-4 manifest");
    fs::write(
        project_5_dir.join("package.json"),
        serde_json::json!({
            "name": "project-5",
            "version": "1.0.0",
            "dependencies": {
                "project-4": "workspace:*"
            },
            "devDependencies": {
                "is-positive": "1.0.0"
            },
            "dependenciesMeta": {
                "project-4": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write project-5 manifest");

    pacquet.with_args(["-C", workspace.to_str().unwrap(), "install", "-r"]).assert().success();

    let project_3_dep = project_3_dir.join("node_modules/project-2");
    assert!(project_3_dep.exists());
    assert!(!is_symlink_or_junction(&project_3_dep).expect("read project-3 dep metadata"));
    assert!(project_3_dep.join("node_modules/project-1").exists());
    assert!(project_3_dep.join("node_modules/project-1/node_modules/is-positive").exists());

    let project_4_dep = project_4_dir.join("node_modules/project-2");
    assert!(project_4_dep.exists());
    assert!(is_symlink_or_junction(&project_4_dep).expect("read project-4 dep metadata"));

    let project_5_dep = project_5_dir.join("node_modules/project-4");
    assert!(project_5_dep.exists());
    assert!(is_symlink_or_junction(&project_5_dep).expect("read project-5 dep metadata"));
    assert!(project_5_dep.join("node_modules/project-2").exists());

    let lockfile_content =
        fs::read_to_string(workspace.join("pnpm-lock.yaml")).expect("read workspace lockfile");
    assert!(lockfile_content.contains("packages/project-3:"));
    assert!(lockfile_content.contains("version: file:packages/project-2(is-positive@2.0.0)"));
    assert!(lockfile_content.contains("project-2@file:packages/project-2(is-positive@2.0.0):"));
    assert!(lockfile_content.contains("packages/project-4:"));
    assert!(lockfile_content.contains("version: link:../project-2"));
    assert!(lockfile_content.contains("packages/project-5:"));
    assert!(lockfile_content.contains("version: link:../project-4"));
    assert!(!lockfile_content.contains("project-2@file:packages/project-2(is-positive@1.0.0):"));
    assert!(!lockfile_content.contains("project-4@file:packages/project-4(is-positive@1.0.0):"));

    drop(root); // cleanup
}

#[test]
fn frozen_workspace_injected_dependency_should_keep_deduped_multilevel_transitive_peer_context() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let store_dir = root.path().join("pacquet-store");
    let cache_dir = root.path().join("pacquet-cache");
    fs::write(
        workspace.join(".npmrc"),
        format!(
            "registry=https://registry.npmjs.org/\nstore-dir={}\ncache-dir={}\nfetch-timeout=1000\n",
            store_dir.display(),
            cache_dir.display()
        ),
    )
    .expect("write .npmrc");

    let project_1_dir = workspace.join("packages/project-1");
    let project_2_dir = workspace.join("packages/project-2");
    let project_4_dir = workspace.join("packages/project-4");
    let project_5_dir = workspace.join("packages/project-5");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
    fs::create_dir_all(&project_4_dir).expect("create project-4 dir");
    fs::create_dir_all(&project_5_dir).expect("create project-5 dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "peerDependencies": {
                "is-positive": ">=1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "dependencies": {
                "project-1": "workspace:*"
            },
            "devDependencies": {
                "is-positive": "1.0.0"
            },
            "dependenciesMeta": {
                "project-1": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");
    fs::write(
        project_4_dir.join("package.json"),
        serde_json::json!({
            "name": "project-4",
            "version": "1.0.0",
            "dependencies": {
                "project-2": "workspace:*"
            },
            "devDependencies": {
                "is-positive": "1.0.0"
            },
            "dependenciesMeta": {
                "project-2": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write project-4 manifest");
    fs::write(
        project_5_dir.join("package.json"),
        serde_json::json!({
            "name": "project-5",
            "version": "1.0.0",
            "dependencies": {
                "project-4": "workspace:*"
            },
            "devDependencies": {
                "is-positive": "1.0.0"
            },
            "dependenciesMeta": {
                "project-4": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write project-5 manifest");

    pacquet.with_args(["-C", workspace.to_str().unwrap(), "install", "-r"]).assert().success();
    fs::remove_dir_all(project_5_dir.join("node_modules")).expect("remove project-5 node_modules");

    pacquet_command(&workspace)
        .with_args(["-C", workspace.to_str().unwrap(), "install", "-r", "--frozen-lockfile"])
        .assert()
        .success();

    let project_5_dep = project_5_dir.join("node_modules/project-4");
    assert!(project_5_dep.exists());
    assert!(is_symlink_or_junction(&project_5_dep).expect("read project-5 dep metadata"));
    assert!(project_5_dep.join("node_modules/project-2").exists());

    drop(root); // cleanup
}

#[test]
fn workspace_injected_dependency_should_refresh_on_reinstall_by_default() {
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
    fs::write(lib_dir.join("index.js"), "module.exports = 'v1';\n").expect("write lib entrypoint");
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
    fs::write(lib_dir.join("index.js"), "module.exports = 'v2';\n").expect("update lib entrypoint");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install"])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(app_dir.join("node_modules/@repo/lib/index.js"))
            .expect("read injected dependency file"),
        "module.exports = 'v2';\n"
    );

    drop(root); // cleanup
}

#[test]
fn workspace_injected_dependency_should_materialize_when_node_linker_is_hoisted() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let project_1_dir = workspace.join("packages/project-1");
    let project_2_dir = workspace.join("packages/project-2");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_2_dir).expect("create project-2 dir");
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
    .expect("write workspace root manifest");
    fs::write(
        workspace.join(".npmrc"),
        format!(
            "node-linker=hoisted\ndedupe-injected-deps=false\nregistry={}\n",
            mock_instance.url()
        ),
    )
    .expect("write workspace npmrc");
    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "main": "index.js",
            "dependencies": {
                "is-number": "7.0.0"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(project_1_dir.join("index.js"), "module.exports = 'project-1';\n")
        .expect("write project-1 entrypoint");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "dependencies": {
                "project-1": "workspace:*"
            },
            "dependenciesMeta": {
                "project-1": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");

    pacquet.with_args(["-C", workspace.to_str().unwrap(), "install", "-r"]).assert().success();

    let injected_dep = project_2_dir.join("node_modules/project-1");
    assert!(injected_dep.exists());
    assert!(!is_symlink_or_junction(&injected_dep).expect("read injected dependency metadata"));
    assert!(injected_dep.join("node_modules/is-number").exists());
    assert!(workspace.join("node_modules/is-number").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_injected_dependency_should_relink_hardlinked_files_on_reinstall() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app package dir");
    fs::create_dir_all(&lib_dir).expect("create lib package dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(app_dir.join(".npmrc"), "package-import-method=hardlink\n").expect("write app npmrc");
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
    fs::write(lib_dir.join("index.js"), "module.exports = 'v1';\n").expect("write lib entrypoint");
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

    let installed_file = app_dir.join("node_modules/@repo/lib/index.js");
    let virtual_store_file = find_virtual_store_file(&app_dir, "@repo/lib", "index.js");
    assert!(same_physical_file(&installed_file, &virtual_store_file));

    fs::remove_file(&installed_file).expect("remove installed file to break hardlink");
    fs::write(&installed_file, "module.exports = 'broken';\n").expect("rewrite installed file");
    assert!(!same_physical_file(&installed_file, &virtual_store_file));

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install"])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(&installed_file).expect("read reinstalled file"),
        "module.exports = 'v1';\n"
    );
    assert!(same_physical_file(&installed_file, &virtual_store_file));

    drop(root); // cleanup
}

#[test]
fn workspace_injected_dependency_should_relink_hardlinked_files_in_multiple_projects_when_hoisted()
{
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_1_dir = workspace.join("packages/app-1");
    let app_2_dir = workspace.join("packages/app-2");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_1_dir).expect("create app-1 package dir");
    fs::create_dir_all(&app_2_dir).expect("create app-2 package dir");
    fs::create_dir_all(&lib_dir).expect("create lib package dir");
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
    .expect("write workspace root manifest");
    fs::write(
        workspace.join(".npmrc"),
        "node-linker=hoisted\npackage-import-method=hardlink\ndedupe-injected-deps=false\n",
    )
    .expect("write workspace npmrc");
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
    fs::write(lib_dir.join("index.js"), "module.exports = 'v1';\n").expect("write lib entrypoint");

    for app_dir in [&app_1_dir, &app_2_dir] {
        fs::write(
            app_dir.join("package.json"),
            serde_json::json!({
                "name": app_dir.file_name().and_then(|name| name.to_str()).expect("app dir name"),
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
    }

    pacquet.with_args(["-C", workspace.to_str().unwrap(), "install", "-r"]).assert().success();

    let virtual_store_file = find_virtual_store_file(&workspace, "@repo/lib", "index.js");
    let installed_1 = app_1_dir.join("node_modules/@repo/lib/index.js");
    let installed_2 = app_2_dir.join("node_modules/@repo/lib/index.js");
    assert!(same_physical_file(&installed_1, &virtual_store_file));
    assert!(same_physical_file(&installed_2, &virtual_store_file));

    fs::remove_file(&installed_1).expect("remove app-1 installed file");
    fs::write(&installed_1, "module.exports = 'broken-1';\n").expect("rewrite app-1 file");
    fs::remove_file(&installed_2).expect("remove app-2 installed file");
    fs::write(&installed_2, "module.exports = 'broken-2';\n").expect("rewrite app-2 file");
    assert!(!same_physical_file(&installed_1, &virtual_store_file));
    assert!(!same_physical_file(&installed_2, &virtual_store_file));

    pacquet_command(&workspace).with_args(["install", "-r"]).assert().success();

    assert_eq!(
        fs::read_to_string(&installed_1).expect("read app-1 reinstalled file"),
        "module.exports = 'v1';\n"
    );
    assert_eq!(
        fs::read_to_string(&installed_2).expect("read app-2 reinstalled file"),
        "module.exports = 'v1';\n"
    );
    assert!(same_physical_file(&installed_1, &virtual_store_file));
    assert!(same_physical_file(&installed_2, &virtual_store_file));

    drop(root); // cleanup
}

#[test]
fn disable_relink_local_dir_deps_should_keep_existing_workspace_injected_dependency_contents() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app package dir");
    fs::create_dir_all(&lib_dir).expect("create lib package dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(app_dir.join(".npmrc"), "disable-relink-local-dir-deps=true\n")
        .expect("write app npmrc");
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
    fs::write(lib_dir.join("index.js"), "module.exports = 'v1';\n").expect("write lib entrypoint");
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
    fs::write(lib_dir.join("index.js"), "module.exports = 'v2';\n").expect("update lib entrypoint");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(app_dir.join("node_modules/@repo/lib/index.js"))
            .expect("read injected dependency file"),
        "module.exports = 'v1';\n"
    );

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install", "--frozen-lockfile"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(app_dir.join("node_modules/@repo/lib/index.js"))
            .expect("read injected dependency file"),
        "module.exports = 'v1';\n"
    );

    drop(root); // cleanup
}

#[test]
fn disable_relink_local_dir_deps_should_not_relink_hardlinked_workspace_injected_dependency() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app package dir");
    fs::create_dir_all(&lib_dir).expect("create lib package dir");
    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    fs::write(
        app_dir.join(".npmrc"),
        "package-import-method=hardlink\ndisable-relink-local-dir-deps=true\n",
    )
    .expect("write app npmrc");
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
    fs::write(lib_dir.join("index.js"), "module.exports = 'v1';\n").expect("write lib entrypoint");
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

    let installed_file = app_dir.join("node_modules/@repo/lib/index.js");
    let virtual_store_file = find_virtual_store_file(&app_dir, "@repo/lib", "index.js");
    assert!(same_physical_file(&installed_file, &virtual_store_file));

    fs::remove_file(&installed_file).expect("remove installed file to break hardlink");
    fs::write(&installed_file, "module.exports = 'broken';\n").expect("rewrite installed file");
    assert!(!same_physical_file(&installed_file, &virtual_store_file));

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install"])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(&installed_file).expect("read unrelinked file"),
        "module.exports = 'broken';\n"
    );
    assert!(!same_physical_file(&installed_file, &virtual_store_file));

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
fn link_protocol_dependency_should_install_with_symlinked_node_modules() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("local-pkg");
    let shared_node_modules = workspace.join("shared-node_modules");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create local-pkg dir");
    fs::create_dir_all(&shared_node_modules).expect("create shared node_modules dir");
    symlink_dir(&shared_node_modules, &app_dir.join("node_modules"))
        .expect("symlink app node_modules to shared directory");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "local-pkg",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write linked package manifest");
    fs::write(lib_dir.join("index.js"), "module.exports = 'linked';\n").expect("write source file");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "local-pkg": "link:../local-pkg"
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    let linked_dep = app_dir.join("node_modules/local-pkg");
    assert!(linked_dep.exists());
    assert!(is_symlink_or_junction(&linked_dep).unwrap());
    assert_eq!(
        fs::read_to_string(linked_dep.join("index.js")).expect("read linked file"),
        "module.exports = 'linked';\n"
    );

    let lockfile_content =
        fs::read_to_string(app_dir.join("pnpm-lock.yaml")).expect("read app lockfile");
    assert!(lockfile_content.contains("specifier: link:../local-pkg"));
    assert!(lockfile_content.contains("version: link:../local-pkg"));

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
    assert!(lockfile_content.contains("'@repo/lib@file:../src':"));
    assert!(lockfile_content.contains("type: directory"));
    assert!(lockfile_content.contains("directory: ../src"));

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
fn reinstall_should_refresh_file_protocol_dependency_snapshot_and_lockfile() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("src");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create src dir");
    fs::write(app_dir.join(".npmrc"), format!("registry={}\n", mock_instance.url()))
        .expect("write app npmrc");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write file package manifest");
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

    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("update source package with dependency");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install"])
        .assert()
        .success();

    assert!(app_dir.join("node_modules/.pnpm/is-positive@1.0.0").exists());
    let mut lockfile_content =
        fs::read_to_string(app_dir.join("pnpm-lock.yaml")).expect("read lockfile after add");
    assert!(lockfile_content.contains("'@repo/lib@file:../src':"));
    assert!(lockfile_content.contains("is-positive: 1.0.0"));

    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "dependencies": {
                "is-positive": "2.0.0"
            }
        })
        .to_string(),
    )
    .expect("update source dependency version");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install"])
        .assert()
        .success();

    assert!(app_dir.join("node_modules/.pnpm/is-positive@2.0.0").exists());
    lockfile_content =
        fs::read_to_string(app_dir.join("pnpm-lock.yaml")).expect("read lockfile after update");
    assert!(lockfile_content.contains("is-positive: 2.0.0"));

    drop(mock_instance);
    drop(root); // cleanup
}

#[test]
fn file_protocol_dependency_should_relink_hardlinked_files_on_reinstall() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("src");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create src dir");
    fs::write(app_dir.join(".npmrc"), "package-import-method=hardlink\n").expect("write app npmrc");
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

    let installed_file = app_dir.join("node_modules/@repo/lib/index.js");
    let virtual_store_file = find_virtual_store_file(&app_dir, "@repo/lib", "index.js");
    assert!(same_physical_file(&installed_file, &virtual_store_file));

    fs::remove_file(&installed_file).expect("remove installed file to break hardlink");
    fs::write(&installed_file, "module.exports = 'broken';\n").expect("rewrite installed file");
    assert!(!same_physical_file(&installed_file, &virtual_store_file));

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install"])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(&installed_file).expect("read reinstalled file"),
        "module.exports = 'v1';\n"
    );
    assert!(same_physical_file(&installed_file, &virtual_store_file));

    drop(root); // cleanup
}

#[test]
fn disable_relink_local_dir_deps_should_keep_existing_file_dependency_contents() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("src");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create src dir");
    fs::write(app_dir.join(".npmrc"), "disable-relink-local-dir-deps=true\n")
        .expect("write app npmrc");
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

    fs::write(lib_dir.join("new.js"), "module.exports = 'new';\n").expect("write new source file");

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install"])
        .assert()
        .success();
    assert!(!app_dir.join("node_modules/@repo/lib/new.js").exists());

    pacquet_command(&workspace)
        .with_args(["-C", app_dir.to_str().unwrap(), "install", "--frozen-lockfile"])
        .assert()
        .success();
    assert!(!app_dir.join("node_modules/@repo/lib/new.js").exists());

    drop(root); // cleanup
}

#[test]
fn file_protocol_dependency_should_write_directory_snapshot_and_install_nested_local_dependency() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let project_1_dir = workspace.join("project-1");
    let project_2_dir = workspace.join("project-2");
    let project_3_dir = project_2_dir.join("project-3");
    fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
    fs::create_dir_all(&project_3_dir).expect("create project-3 dir");

    fs::write(
        project_1_dir.join("package.json"),
        serde_json::json!({
            "name": "project-1",
            "version": "1.0.0",
            "dependencies": {
                "project-2": "file:../project-2"
            }
        })
        .to_string(),
    )
    .expect("write project-1 manifest");
    fs::write(
        project_2_dir.join("package.json"),
        serde_json::json!({
            "name": "project-2",
            "version": "1.0.0",
            "dependencies": {
                "project-3": "file:./project-3"
            }
        })
        .to_string(),
    )
    .expect("write project-2 manifest");
    fs::write(project_2_dir.join("index.js"), "module.exports = 'project-2';\n")
        .expect("write project-2 file");
    fs::write(
        project_3_dir.join("package.json"),
        serde_json::json!({
            "name": "project-3",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write project-3 manifest");
    fs::write(project_3_dir.join("index.js"), "module.exports = 'project-3';\n")
        .expect("write project-3 file");

    pacquet.with_args(["-C", project_1_dir.to_str().unwrap(), "install"]).assert().success();

    assert!(project_1_dir.join("node_modules/project-2").exists());
    assert!(project_1_dir.join("node_modules/project-2/node_modules/project-3").exists());

    let lockfile_content =
        fs::read_to_string(project_1_dir.join("pnpm-lock.yaml")).expect("read lockfile");
    assert!(lockfile_content.contains("project-2@file:../project-2:"));
    assert!(lockfile_content.contains("project-3@file:../project-2/project-3:"));
    assert!(lockfile_content.contains("directory: ../project-2"));
    assert!(lockfile_content.contains("directory: ../project-2/project-3"));
    assert!(lockfile_content.contains("project-3:"));
    assert!(lockfile_content.contains("file:../project-2/project-3"));

    drop(root); // cleanup
}

#[test]
fn file_protocol_injected_dependency_should_write_peer_suffixed_local_snapshot() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("src");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create src dir");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "local-peer",
            "version": "1.0.0",
            "main": "index.js",
            "peerDependencies": {
                "is-number": "^7.0.0"
            }
        })
        .to_string(),
    )
    .expect("write local peer package manifest");
    fs::write(lib_dir.join("index.js"), "module.exports = 'local-peer';\n")
        .expect("write local peer entrypoint");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "local-peer": "file:../src"
            },
            "devDependencies": {
                "is-number": "7.0.0"
            },
            "dependenciesMeta": {
                "local-peer": {
                    "injected": true
                }
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();

    let materialized_dep = app_dir.join("node_modules/local-peer");
    assert!(materialized_dep.exists());
    assert!(materialized_dep.join("node_modules/is-number").exists());

    let lockfile_content =
        fs::read_to_string(app_dir.join("pnpm-lock.yaml")).expect("read app lockfile");
    assert!(lockfile_content.contains("version: file:../src(is-number@7.0.0)"));
    assert!(lockfile_content.contains("local-peer@file:../src(is-number@7.0.0):"));
    assert!(lockfile_content.contains("peerDependencies:"));
    assert!(lockfile_content.contains("is-number: ^7.0.0"));

    drop(mock_instance);
    drop(root); // cleanup
}

#[test]
fn frozen_file_protocol_injected_dependency_should_materialize_peer_variant_from_lockfile() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("src");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create src dir");
    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "local-peer",
            "version": "1.0.0",
            "main": "index.js",
            "peerDependencies": {
                "is-number": "^7.0.0"
            }
        })
        .to_string(),
    )
    .expect("write local peer package manifest");
    fs::write(lib_dir.join("index.js"), "module.exports = 'local-peer';\n")
        .expect("write local peer entrypoint");
    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0",
            "dependencies": {
                "local-peer": "file:../src"
            },
            "devDependencies": {
                "is-number": "7.0.0"
            },
            "dependenciesMeta": {
                "local-peer": {
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

    let materialized_dep = app_dir.join("node_modules/local-peer");
    assert!(materialized_dep.exists());
    assert!(materialized_dep.join("node_modules/is-number").exists());

    drop(mock_instance);
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
