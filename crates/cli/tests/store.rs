use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_store_dir::PackageFilesIndex;
use pacquet_testing_utils::bin::CommandTempCwd;
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Handle the slight difference between OSes.
///
/// **TODO:** may be we should have handle them in the production code instead?
fn canonicalize(path: &Path) -> PathBuf {
    if cfg!(windows) {
        path.to_path_buf()
    } else {
        dunce::canonicalize(path).expect("canonicalize path")
    }
}

fn first_index_path(store_dir: &Path) -> PathBuf {
    fn walk(dir: &Path, result: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                walk(&path, result);
                continue;
            }
            if file_type.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json")
            {
                result.push(path);
            }
        }
    }

    let mut files = Vec::new();
    walk(&store_dir.join("v10/index"), &mut files);
    files.sort();
    files.into_iter().next().expect("index file should exist")
}

fn package_ids_from_index_files(store_dir: &Path) -> Vec<String> {
    store_dir
        .join("v10/index")
        .pipe(walkdir::WalkDir::new)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
        .filter_map(|entry| {
            let file = fs::File::open(entry.path()).ok()?;
            let index: PackageFilesIndex = serde_json::from_reader(file).ok()?;
            Some(format!("{}@{}", index.name?, index.version?))
        })
        .collect()
}

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn store_status_should_succeed_for_unmodified_store() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;

    pacquet_command(&workspace)
        .with_args(["add", "@pnpm.e2e/hello-world-js-bin@1.0.0"])
        .assert()
        .success();

    let output = pacquet_command(&workspace)
        .with_args(["store", "status"])
        .output()
        .expect("run pacquet store status");
    assert!(output.status.success());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn store_status_should_fail_when_store_entry_is_missing() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let store_dir = npmrc_info.store_dir.clone();
    let mock_instance = npmrc_info.mock_instance;

    pacquet_command(&workspace)
        .with_args(["add", "@pnpm.e2e/hello-world-js-bin@1.0.0"])
        .assert()
        .success();

    let index_path = first_index_path(&store_dir);
    let index_file = fs::File::open(&index_path).expect("open index file");
    let index: PackageFilesIndex = serde_json::from_reader(index_file).expect("parse index");
    let file_info = index.files.values().next().expect("file entry in index");
    let executable = (file_info.mode & 0o111) != 0;
    let cas_path = pacquet_store_dir::StoreDir::new(&store_dir)
        .cas_file_path_by_integrity(&file_info.integrity, executable)
        .expect("resolve cas path");
    fs::remove_file(&cas_path).expect("remove cas file");

    let output = pacquet_command(&workspace)
        .with_args(["store", "status"])
        .output()
        .expect("run pacquet store status");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("store has modified packages"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn store_add_should_warm_store_without_modifying_workspace() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let store_dir = npmrc_info.store_dir.clone();
    let mock_instance = npmrc_info.mock_instance;

    assert!(!workspace.join("package.json").exists());
    assert!(!workspace.join("node_modules").exists());
    assert!(!workspace.join("pnpm-lock.yaml").exists());

    pacquet_command(&workspace)
        .with_args(["store", "add", "@pnpm.e2e/hello-world-js-bin@1.0.0"])
        .assert()
        .success();

    assert!(first_index_path(&store_dir).is_file());
    assert!(!workspace.join("package.json").exists());
    assert!(!workspace.join("node_modules").exists());
    assert!(!workspace.join("pnpm-lock.yaml").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn store_add_should_fail_without_package_specs() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;

    let output = pacquet_command(&workspace)
        .with_args(["store", "add"])
        .output()
        .expect("run pacquet store add");
    assert!(!output.status.success());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn store_prune_should_remove_only_unreferenced_packages() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let store_dir = npmrc_info.store_dir.clone();
    let mock_instance = npmrc_info.mock_instance;

    pacquet_command(&workspace)
        .with_args(["add", "@pnpm.e2e/hello-world-js-bin@1.0.0"])
        .assert()
        .success();
    pacquet_command(&workspace).with_args(["store", "add", "@pnpm/xyz@1.0.0"]).assert().success();

    let mut before_ids = package_ids_from_index_files(&store_dir);
    before_ids.sort();
    assert!(before_ids.contains(&"@pnpm.e2e/hello-world-js-bin@1.0.0".to_string()));
    assert!(before_ids.contains(&"@pnpm/xyz@1.0.0".to_string()));
    let project_registry_entries =
        fs::read_dir(store_dir.join("v10/projects")).expect("read store project registry").count();
    assert!(project_registry_entries >= 1);
    assert!(workspace.join("node_modules/.pnpm").is_dir());

    pacquet_command(&workspace).with_args(["store", "prune"]).assert().success();

    let mut after_ids = package_ids_from_index_files(&store_dir);
    after_ids.sort();
    assert!(after_ids.contains(&"@pnpm.e2e/hello-world-js-bin@1.0.0".to_string()));
    assert!(!after_ids.contains(&"@pnpm/xyz@1.0.0".to_string()));

    pacquet_command(&workspace).with_args(["store", "status"]).assert().success();

    drop((root, mock_instance)); // cleanup
}

#[test]
fn store_path_should_return_store_dir_from_npmrc() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    eprintln!("Creating .npmrc...");
    fs::write(workspace.join(".npmrc"), "store-dir=foo/bar").expect("write to .npmrc");

    eprintln!("Executing pacquet store path...");
    let output = pacquet.with_args(["store", "path"]).output().expect("run pacquet store path");
    dbg!(&output);

    eprintln!("Exit status code");
    assert!(output.status.success());

    eprintln!("Stdout");
    let normalize = |path: &str| path.replace('\\', "/");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end().pipe(normalize),
        canonicalize(&workspace).join("foo/bar").to_string_lossy().pipe_as_ref(normalize),
    );

    drop(root); // cleanup
}
