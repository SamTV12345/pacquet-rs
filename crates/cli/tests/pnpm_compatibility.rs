#![cfg(unix)] // running this on windows result in 'program not found'
pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::{
    bin::{AddMockedRegistry, CommandTempCwd},
    fs::get_all_files,
};
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use serde_json::Value as JsonValue;
use std::{collections::BTreeMap, fs, path::Path};
use walkdir::WalkDir;

fn normalize_store_files(files: &[String]) -> Vec<String> {
    let mut normalized = files
        .iter()
        .filter_map(|path| {
            let path = path.replace('\\', "/");
            let (_, suffix) = path.split_once("/files/")?;
            // pnpm v10 stores package index metadata in v10/index/*, not in files/*-index.json.
            // Pacquet currently stores index metadata in files/*-index.json.
            // For cross-tool compatibility we only compare CAS payload files here.
            if suffix.ends_with("-index.json") {
                return None;
            }
            Some(format!("files/{suffix}"))
        })
        .collect::<Vec<_>>();
    normalized.sort();
    normalized
}

fn parse_index_payload_file(path: &Path) -> Option<BTreeMap<String, JsonValue>> {
    let path_str = path.to_string_lossy().replace('\\', "/");

    if path_str.ends_with("-index.json") {
        let parsed = fs::File::open(path).ok().and_then(|file| {
            serde_json::from_reader::<_, pacquet_store_dir::PackageFilesIndex>(file).ok()
        })?;
        let mut files = BTreeMap::<String, JsonValue>::new();
        for (name, mut info) in parsed.files {
            info.checked_at = None;
            files.insert(name, serde_json::to_value(info).expect("serialize package file info"));
        }
        return Some(files);
    }

    if path_str.contains("/index/") && path_str.ends_with(".json") {
        let value = fs::File::open(path)
            .ok()
            .and_then(|file| serde_json::from_reader::<_, JsonValue>(file).ok())?;
        let file_entries = value.get("files")?.as_object()?;
        let mut files = BTreeMap::<String, JsonValue>::new();
        for (name, info) in file_entries {
            let mut object = info.as_object()?.clone();
            object.remove("checkedAt");
            files.insert(name.clone(), JsonValue::Object(object));
        }
        return Some(files);
    }

    None
}

fn normalized_index_payloads(store_dir: &Path) -> Vec<JsonValue> {
    let mut payloads = WalkDir::new(store_dir)
        .into_iter()
        .map(|entry| entry.expect("walk store dir entry"))
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| parse_index_payload_file(entry.path()))
        .map(|payload| serde_json::to_value(payload).expect("serialize normalized index payload"))
        .collect::<Vec<_>>();
    payloads.sort_by_key(|value| value.to_string());
    payloads
}

#[test]
#[ignore = "requires metadata cache feature which pacquet doesn't yet have"]
fn store_usable_by_pnpm_offline() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Using pacquet to populate the store...");
    pacquet.with_arg("install").assert().success();
    fs::remove_dir_all(workspace.join("node_modules")).expect("delete node_modules");

    eprintln!("pnpm install --offline --ignore-scripts");
    pnpm.with_args(["install", "--offline", "--ignore-scripts"]).assert().success();

    drop((root, mock_instance)); // cleanup
}

#[test]
fn same_file_structure() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    let modules_dir = workspace.join("node_modules");
    let cleanup = || {
        eprintln!("Cleaning up...");
        fs::remove_dir_all(&store_dir).expect("delete store dir");
        fs::remove_dir_all(&modules_dir).expect("delete node_modules");
    };

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Installing with pacquet...");
    pacquet.with_arg("install").assert().success();
    let pacquet_store_files = normalize_store_files(&get_all_files(&store_dir));

    cleanup();

    eprintln!("Installing with pnpm...");
    pnpm.with_args(["install", "--ignore-scripts"]).assert().success();
    let pnpm_store_files = normalize_store_files(&get_all_files(&store_dir));

    cleanup();

    eprintln!("Produce the same CAS file structure");
    assert_eq!(&pacquet_store_files, &pnpm_store_files);

    drop((root, mock_instance)); // cleanup
}

#[test]
fn same_index_file_contents() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    let modules_dir = workspace.join("node_modules");
    let cleanup = || {
        eprintln!("Cleaning up...");
        fs::remove_dir_all(&store_dir).expect("delete store dir");
        fs::remove_dir_all(&modules_dir).expect("delete node_modules");
    };

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Installing with pacquet...");
    pacquet.with_arg("install").assert().success();
    let pacquet_index_file_contents = normalized_index_payloads(&store_dir);

    cleanup();

    eprintln!("Installing with pnpm...");
    pnpm.with_args(["install", "--ignore-scripts"]).assert().success();
    let pnpm_index_file_contents = normalized_index_payloads(&store_dir);

    cleanup();

    eprintln!("Produce equivalent index payloads");
    assert_eq!(&pacquet_index_file_contents, &pnpm_index_file_contents);

    drop((root, mock_instance)); // cleanup
}
