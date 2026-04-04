#![cfg(unix)] // running this on windows result in 'program not found'
pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::{
    bin::{AddMockedRegistry, CommandTempCwd},
    fs::get_all_files,
};
use pretty_assertions::assert_eq;
use std::{collections::BTreeMap, fs, io::Write, path::Path};
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ComparableFileInfo {
    integrity: String,
    mode: u32,
    size: Option<u64>,
}

fn normalize_store_files(files: &[String]) -> Vec<String> {
    let mut normalized = files
        .iter()
        .filter_map(|path| {
            let path = path.replace('\\', "/");
            if let Some((_, suffix)) = path.split_once("/files/") {
                return Some(format!("files/{suffix}"));
            }
            if let Some((_, suffix)) = path.split_once("/index/") {
                return Some(format!("index/{suffix}"));
            }
            None
        })
        .collect::<Vec<_>>();
    normalized.sort();
    normalized
}

fn normalize_private_virtual_store_files(files: &[String]) -> Vec<String> {
    let mut normalized = files
        .iter()
        .filter_map(|path| {
            let path = path.replace('\\', "/");
            path.split_once("/node_modules/.pnpm/").map(|(_, suffix)| suffix.to_string())
        })
        .collect::<Vec<_>>();
    normalized.sort();
    normalized
}

fn parse_index_payload_file(path: &Path) -> Option<BTreeMap<String, ComparableFileInfo>> {
    let path_str = path.to_string_lossy().replace('\\', "/");

    if path_str.ends_with("-index.json") {
        let parsed = fs::File::open(path).ok().and_then(|file| {
            serde_json::from_reader::<_, pacquet_store_dir::PackageFilesIndex>(file).ok()
        })?;
        let mut files = BTreeMap::<String, ComparableFileInfo>::new();
        for (name, mut info) in parsed.files {
            info.checked_at = None;
            files.insert(
                name,
                ComparableFileInfo { integrity: info.integrity, mode: info.mode, size: info.size },
            );
        }
        return Some(files);
    }

    if path_str.contains("/index/") && path_str.ends_with(".json") {
        let value = fs::File::open(path)
            .ok()
            .and_then(|file| serde_json::from_reader::<_, serde_json::Value>(file).ok())?;
        let file_entries = value.get("files")?.as_object()?;
        let mut files = BTreeMap::<String, ComparableFileInfo>::new();
        for (name, info) in file_entries {
            let mut info =
                serde_json::from_value::<pacquet_store_dir::PackageFileInfo>(info.clone()).ok()?;
            info.checked_at = None;
            files.insert(
                name.clone(),
                ComparableFileInfo { integrity: info.integrity, mode: info.mode, size: info.size },
            );
        }
        return Some(files);
    }

    None
}

fn normalized_index_payloads(store_dir: &Path) -> Vec<BTreeMap<String, ComparableFileInfo>> {
    let mut payloads = WalkDir::new(store_dir)
        .into_iter()
        .map(|entry| entry.expect("walk store dir entry"))
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| parse_index_payload_file(entry.path()))
        .collect::<Vec<_>>();
    payloads.sort_by_key(|payload| {
        payload
            .iter()
            .map(|(name, info)| {
                format!("{name}|{}|{}|{}", info.integrity, info.mode, info.size.unwrap_or_default())
            })
            .collect::<Vec<_>>()
            .join("||")
    });
    payloads
}

fn normalized_lockfile(lockfile_text: &str) -> serde_json::Value {
    fn canonicalize_json(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut sorted = std::collections::BTreeMap::new();
                for (key, value) in map {
                    sorted.insert(key, canonicalize_json(value));
                }
                let mut canonical = serde_json::Map::new();
                for (key, value) in sorted {
                    canonical.insert(key, value);
                }
                serde_json::Value::Object(canonical)
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonicalize_json).collect())
            }
            other => other,
        }
    }

    let mut value =
        serde_yaml::from_str::<serde_yaml::Value>(lockfile_text).expect("parse lockfile YAML");

    let Some(root) = value.as_mapping_mut() else {
        return canonicalize_json(serde_json::to_value(value).expect("convert yaml to json"));
    };
    let settings_key = serde_yaml::Value::String("settings".to_string());
    let Some(settings) = root.get_mut(&settings_key).and_then(serde_yaml::Value::as_mapping_mut)
    else {
        return canonicalize_json(serde_json::to_value(value).expect("convert yaml to json"));
    };

    let remove_if = |settings: &mut serde_yaml::Mapping, key: &str, expected: serde_yaml::Value| {
        let map_key = serde_yaml::Value::String(key.to_string());
        if settings.get(&map_key) == Some(&expected) {
            settings.remove(&map_key);
        }
    };

    remove_if(settings, "autoInstallPeers", serde_yaml::Value::Bool(true));
    remove_if(settings, "excludeLinksFromLockfile", serde_yaml::Value::Bool(false));
    remove_if(
        settings,
        "peersSuffixMaxLength",
        serde_yaml::Value::Number(serde_yaml::Number::from(1000_u64)),
    );
    remove_if(settings, "injectWorkspacePackages", serde_yaml::Value::Bool(false));

    if settings.is_empty() {
        root.remove(&settings_key);
    }

    canonicalize_json(serde_json::to_value(value).expect("convert yaml to json"))
}

fn normalized_yaml(yaml_text: &str) -> serde_json::Value {
    fn canonicalize_json(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut sorted = std::collections::BTreeMap::new();
                for (key, value) in map {
                    sorted.insert(key, canonicalize_json(value));
                }
                let mut canonical = serde_json::Map::new();
                for (key, value) in sorted {
                    canonical.insert(key, value);
                }
                serde_json::Value::Object(canonical)
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonicalize_json).collect())
            }
            other => other,
        }
    }

    let mut value = serde_yaml::from_str::<serde_yaml::Value>(yaml_text).expect("parse yaml");
    if let Some(root) = value.as_mapping_mut() {
        root.remove(serde_yaml::Value::String("prunedAt".to_string()));
    }
    canonicalize_json(serde_json::to_value(value).expect("convert yaml to json"))
}

fn write_manifest(workspace: &Path, manifest: serde_json::Value) {
    fs::write(workspace.join("package.json"), manifest.to_string()).expect("write to package.json");
}

#[derive(Clone)]
struct RegistryLockfileCase {
    name: &'static str,
    manifest: serde_json::Value,
    npmrc_append: Option<&'static str>,
    workspace_yaml: Option<&'static str>,
    pnpmfile_cjs: Option<&'static str>,
}

fn remove_path_if_exists(path: &Path) {
    if !path.exists() {
        return;
    }
    if path.is_dir() {
        fs::remove_dir_all(path).expect("remove dir");
    } else {
        fs::remove_file(path).expect("remove file");
    }
}

fn assert_same_lockfile_for_registry_case(case: RegistryLockfileCase) {
    let RegistryLockfileCase {
        name: case_name,
        manifest,
        npmrc_append,
        workspace_yaml,
        pnpmfile_cjs,
    } = case;

    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, npmrc_path, .. } = npmrc_info;

    let modules_dir = workspace.join("node_modules");
    let lockfile_path = workspace.join("pnpm-lock.yaml");
    let cleanup = || {
        eprintln!("Cleaning up case: {case_name}");
        remove_path_if_exists(&store_dir);
        remove_path_if_exists(&modules_dir);
        remove_path_if_exists(&lockfile_path);
    };

    write_manifest(&workspace, manifest);
    if let Some(content) = workspace_yaml {
        fs::write(workspace.join("pnpm-workspace.yaml"), content)
            .expect("write pnpm-workspace.yaml");
    }
    if let Some(content) = pnpmfile_cjs {
        fs::write(workspace.join(".pnpmfile.cjs"), content).expect("write .pnpmfile.cjs");
    }
    if let Some(content) = npmrc_append {
        fs::OpenOptions::new()
            .append(true)
            .open(npmrc_path)
            .expect("open .npmrc")
            .write_all(content.as_bytes())
            .expect("append to .npmrc");
    }

    eprintln!("[{case_name}] Installing with pacquet...");
    pacquet.with_arg("install").assert().success();
    let pacquet_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pacquet lockfile"));

    cleanup();

    eprintln!("[{case_name}] Installing with pnpm...");
    pnpm.with_args(["install", "--ignore-scripts"]).assert().success();
    let pnpm_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pnpm lockfile"));

    cleanup();
    assert_eq!(&pacquet_lockfile, &pnpm_lockfile);

    drop((root, mock_instance)); // cleanup
}

fn assert_same_lockfile_for_workspace_manifest(case_name: &str, workspace_spec: &str) {
    let CommandTempCwd { pacquet, pnpm, root, workspace, .. } = CommandTempCwd::init();

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
                "@repo/lib": workspace_spec
            }
        })
        .to_string(),
    )
    .expect("write app package manifest");

    let lockfile_path = workspace.join("pnpm-lock.yaml");
    let cleanup = || {
        eprintln!("Cleaning up case: {case_name}");
        remove_path_if_exists(&workspace.join("node_modules"));
        remove_path_if_exists(&app_dir.join("node_modules"));
        remove_path_if_exists(&lockfile_path);
    };

    eprintln!("[{case_name}] Installing with pacquet...");
    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();
    let pacquet_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pacquet lockfile"));

    cleanup();

    eprintln!("[{case_name}] Installing with pnpm...");
    pnpm.with_args(["-C", app_dir.to_str().unwrap(), "install", "--ignore-scripts"])
        .assert()
        .success();
    let pnpm_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pnpm lockfile"));

    cleanup();
    assert_eq!(&pacquet_lockfile, &pnpm_lockfile);

    drop(root); // cleanup
}

#[test]
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
fn lockfile_only_behaves_like_pnpm() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    let modules_dir = workspace.join("node_modules");
    let lockfile_path = workspace.join("pnpm-lock.yaml");
    let cleanup = || {
        remove_path_if_exists(&store_dir);
        remove_path_if_exists(&modules_dir);
        remove_path_if_exists(&lockfile_path);
    };

    write_manifest(
        &workspace,
        serde_json::json!({
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
            }
        }),
    );

    pacquet.with_args(["install", "--lockfile-only"]).assert().success();
    assert!(lockfile_path.exists());
    assert!(!modules_dir.exists());
    let pacquet_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pacquet lockfile"));

    cleanup();

    pnpm.with_args(["install", "--lockfile-only", "--ignore-scripts"]).assert().success();
    assert!(lockfile_path.exists());
    assert!(!modules_dir.exists());
    let pnpm_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pnpm lockfile"));

    cleanup();

    assert_eq!(&pacquet_lockfile, &pnpm_lockfile);

    drop((root, mock_instance)); // cleanup
}

#[test]
fn resolution_only_behaves_like_pnpm() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    let modules_dir = workspace.join("node_modules");
    let lockfile_path = workspace.join("pnpm-lock.yaml");
    let cleanup = || {
        remove_path_if_exists(&store_dir);
        remove_path_if_exists(&modules_dir);
        remove_path_if_exists(&lockfile_path);
    };

    write_manifest(
        &workspace,
        serde_json::json!({
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
            }
        }),
    );

    pacquet.with_args(["install", "--resolution-only"]).assert().success();
    assert!(lockfile_path.exists());
    assert!(!modules_dir.exists());
    let pacquet_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pacquet lockfile"));

    cleanup();

    pnpm.with_args(["install", "--resolution-only", "--ignore-scripts"]).assert().success();
    assert!(lockfile_path.exists());
    assert!(!modules_dir.exists());
    let pnpm_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pnpm lockfile"));

    cleanup();

    assert_eq!(&pacquet_lockfile, &pnpm_lockfile);

    drop((root, mock_instance)); // cleanup
}

#[test]
fn fix_lockfile_with_frozen_behaves_like_pnpm() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    let modules_dir = workspace.join("node_modules");
    let lockfile_path = workspace.join("pnpm-lock.yaml");
    let cleanup = || {
        remove_path_if_exists(&store_dir);
        remove_path_if_exists(&modules_dir);
        remove_path_if_exists(&lockfile_path);
    };

    write_manifest(
        &workspace,
        serde_json::json!({
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
            }
        }),
    );

    pacquet.with_args(["install", "--frozen-lockfile", "--fix-lockfile"]).assert().success();
    assert!(lockfile_path.exists());
    assert!(modules_dir.exists());
    let pacquet_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pacquet lockfile"));

    cleanup();

    pnpm.with_args(["install", "--frozen-lockfile", "--fix-lockfile", "--ignore-scripts"])
        .assert()
        .success();
    assert!(lockfile_path.exists());
    assert!(modules_dir.exists());
    let pnpm_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pnpm lockfile"));

    cleanup();

    assert_eq!(&pacquet_lockfile, &pnpm_lockfile);

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
        remove_path_if_exists(&store_dir);
        remove_path_if_exists(&modules_dir);
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
        remove_path_if_exists(&store_dir);
        remove_path_if_exists(&modules_dir);
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

#[test]
fn same_modules_manifest_content() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    let modules_dir = workspace.join("node_modules");
    let modules_manifest_path = modules_dir.join(".modules.yaml");
    let cleanup = || {
        eprintln!("Cleaning up...");
        remove_path_if_exists(&store_dir);
        remove_path_if_exists(&modules_dir);
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
    let pacquet_modules_manifest = normalized_yaml(
        &fs::read_to_string(&modules_manifest_path).expect("read pacquet modules manifest"),
    );

    cleanup();

    eprintln!("Installing with pnpm...");
    pnpm.with_args(["install", "--ignore-scripts"]).assert().success();
    let pnpm_modules_manifest = normalized_yaml(
        &fs::read_to_string(&modules_manifest_path).expect("read pnpm modules manifest"),
    );

    cleanup();

    eprintln!("Produce equivalent node_modules/.modules.yaml");
    assert_eq!(&pacquet_modules_manifest, &pnpm_modules_manifest);

    drop((root, mock_instance)); // cleanup
}

#[test]
fn same_private_virtual_store_layout() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    let modules_dir = workspace.join("node_modules");
    let cleanup = || {
        eprintln!("Cleaning up...");
        remove_path_if_exists(&store_dir);
        remove_path_if_exists(&modules_dir);
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
    let pacquet_virtual_store_files = normalize_private_virtual_store_files(&get_all_files(
        &workspace.join("node_modules/.pnpm"),
    ));

    cleanup();

    eprintln!("Installing with pnpm...");
    pnpm.with_args(["install", "--ignore-scripts"]).assert().success();
    let pnpm_virtual_store_files = normalize_private_virtual_store_files(&get_all_files(
        &workspace.join("node_modules/.pnpm"),
    ));

    cleanup();

    eprintln!("Produce equivalent node_modules/.pnpm layout");
    assert_eq!(&pacquet_virtual_store_files, &pnpm_virtual_store_files);

    drop((root, mock_instance)); // cleanup
}

#[test]
fn second_install_is_noop_like_pnpm() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
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

    // Run pacquet twice back-to-back so the second install sees the
    // persisted state (node_modules/.modules.yaml and .pnpm/lock.yaml)
    // that pacquet itself wrote.  Running pnpm in between would overwrite
    // that state and break the fast-path check.
    eprintln!("Priming pacquet install...");
    std::process::Command::new(assert_cmd::cargo::cargo_bin!("pacquet"))
        .with_current_dir(&workspace)
        .with_arg("install")
        .assert()
        .success();

    eprintln!("Running second pacquet install...");
    let pacquet_output = std::process::Command::new(assert_cmd::cargo::cargo_bin!("pacquet"))
        .with_current_dir(&workspace)
        .with_arg("install")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let pacquet_stdout = String::from_utf8(pacquet_output).expect("pacquet stdout utf8");

    // Now do the same for pnpm: prime first, then capture the noop output.
    eprintln!("Priming pnpm install...");
    std::process::Command::new("pnpm")
        .with_current_dir(&workspace)
        .with_args(["install", "--ignore-scripts"])
        .assert()
        .success();

    eprintln!("Running second pnpm install...");
    let pnpm_output = std::process::Command::new("pnpm")
        .with_current_dir(&workspace)
        .with_args(["install", "--ignore-scripts"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let pnpm_stdout = String::from_utf8(pnpm_output).expect("pnpm stdout utf8");

    assert!(pnpm_stdout.contains("Already up to date"));
    assert!(pacquet_stdout.contains("Already up to date"));
    assert!(!pacquet_stdout.contains("Packages: +"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn same_lockfile_content() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    let modules_dir = workspace.join("node_modules");
    let lockfile_path = workspace.join("pnpm-lock.yaml");
    let cleanup = || {
        eprintln!("Cleaning up...");
        remove_path_if_exists(&store_dir);
        remove_path_if_exists(&modules_dir);
        remove_path_if_exists(&lockfile_path);
    };

    eprintln!("Creating package.json...");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    write_manifest(&workspace, package_json_content);

    eprintln!("Installing with pacquet...");
    pacquet.with_arg("install").assert().success();
    let pacquet_lockfile = fs::read_to_string(&lockfile_path).expect("read pacquet lockfile");
    let pacquet_lockfile = normalized_lockfile(&pacquet_lockfile);

    cleanup();

    eprintln!("Installing with pnpm...");
    pnpm.with_args(["install", "--ignore-scripts"]).assert().success();
    let pnpm_lockfile = fs::read_to_string(&lockfile_path).expect("read pnpm lockfile");
    let pnpm_lockfile = normalized_lockfile(&pnpm_lockfile);

    cleanup();

    eprintln!("Produce equivalent pnpm-lock.yaml");
    assert_eq!(&pacquet_lockfile, &pnpm_lockfile);

    drop((root, mock_instance)); // cleanup
}

#[test]
fn same_lockfile_content_with_dev_dependencies() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    let modules_dir = workspace.join("node_modules");
    let lockfile_path = workspace.join("pnpm-lock.yaml");
    let cleanup = || {
        eprintln!("Cleaning up...");
        remove_path_if_exists(&store_dir);
        remove_path_if_exists(&modules_dir);
        remove_path_if_exists(&lockfile_path);
    };

    eprintln!("Creating package.json...");
    write_manifest(
        &workspace,
        serde_json::json!({
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
            },
            "devDependencies": {
                "@pnpm.e2e/hello-world-js-bin": "1.0.0",
            }
        }),
    );

    eprintln!("Installing with pacquet...");
    pacquet.with_arg("install").assert().success();
    let pacquet_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pacquet lockfile"));

    cleanup();

    eprintln!("Installing with pnpm...");
    pnpm.with_args(["install", "--ignore-scripts"]).assert().success();
    let pnpm_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pnpm lockfile"));

    cleanup();

    eprintln!("Produce equivalent pnpm-lock.yaml");
    assert_eq!(&pacquet_lockfile, &pnpm_lockfile);

    drop((root, mock_instance)); // cleanup
}

#[test]
fn same_lockfile_content_for_workspace_link() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, .. } = CommandTempCwd::init();

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

    let lockfile_path = workspace.join("pnpm-lock.yaml");
    let cleanup = || {
        eprintln!("Cleaning up...");
        remove_path_if_exists(&workspace.join("node_modules"));
        remove_path_if_exists(&app_dir.join("node_modules"));
        remove_path_if_exists(&lockfile_path);
    };

    eprintln!("Installing with pacquet...");
    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "install"]).assert().success();
    let pacquet_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pacquet lockfile"));

    cleanup();

    eprintln!("Installing with pnpm...");
    pnpm.with_args(["-C", app_dir.to_str().unwrap(), "install", "--ignore-scripts"])
        .assert()
        .success();
    let pnpm_lockfile =
        normalized_lockfile(&fs::read_to_string(&lockfile_path).expect("read pnpm lockfile"));

    cleanup();

    eprintln!("Produce equivalent workspace pnpm-lock.yaml");
    assert_eq!(&pacquet_lockfile, &pnpm_lockfile);

    drop(root); // cleanup
}

#[test]
fn golden_lockfile_suite_matrix_registry() {
    let cases = vec![
        RegistryLockfileCase {
            name: "prod-only",
            manifest: serde_json::json!({
                "dependencies": {
                    "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
                }
            }),
            npmrc_append: None,
            workspace_yaml: None,
            pnpmfile_cjs: None,
        },
        RegistryLockfileCase {
            name: "prod-and-dev",
            manifest: serde_json::json!({
                "dependencies": {
                    "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
                },
                "devDependencies": {
                    "@pnpm.e2e/hello-world-js-bin": "1.0.0",
                }
            }),
            npmrc_append: None,
            workspace_yaml: None,
            pnpmfile_cjs: None,
        },
        RegistryLockfileCase {
            name: "prod-and-optional",
            manifest: serde_json::json!({
                "dependencies": {
                    "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
                },
                "optionalDependencies": {
                    "@pnpm.e2e/hello-world-js-bin": "1.0.0",
                }
            }),
            npmrc_append: None,
            workspace_yaml: None,
            pnpmfile_cjs: None,
        },
        RegistryLockfileCase {
            name: "peer-dependencies",
            manifest: serde_json::json!({
                "dependencies": {
                    "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
                },
                "peerDependencies": {
                    "@pnpm.e2e/hello-world-js-bin": "1.0.0",
                }
            }),
            npmrc_append: Some("\nauto-install-peers=false\n"),
            workspace_yaml: None,
            pnpmfile_cjs: None,
        },
        RegistryLockfileCase {
            name: "overrides",
            manifest: serde_json::json!({
                "dependencies": {
                    "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
                },
                "pnpm": {
                    "overrides": {
                        "@pnpm.e2e/hello-world-js-bin": "1.0.0",
                    }
                }
            }),
            npmrc_append: None,
            workspace_yaml: None,
            pnpmfile_cjs: None,
        },
        RegistryLockfileCase {
            name: "package-extensions",
            manifest: serde_json::json!({
                "dependencies": {
                    "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
                },
                "pnpm": {
                    "packageExtensions": {
                        "@pnpm.e2e/hello-world-js-bin-parent@1.0.0": {
                            "dependencies": {
                                "@pnpm.e2e/hello-world-js-bin": "1.0.0"
                            }
                        }
                    }
                }
            }),
            npmrc_append: None,
            workspace_yaml: None,
            pnpmfile_cjs: None,
        },
        RegistryLockfileCase {
            name: "catalog",
            manifest: serde_json::json!({
                "dependencies": {
                    "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
                }
            }),
            npmrc_append: None,
            workspace_yaml: Some("packages:\n  - .\ncatalog:\n  hello: 1.0.0\n"),
            pnpmfile_cjs: None,
        },
        RegistryLockfileCase {
            name: "pnpmfile-checksum",
            manifest: serde_json::json!({
                "dependencies": {
                    "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
                }
            }),
            npmrc_append: None,
            workspace_yaml: None,
            pnpmfile_cjs: Some("module.exports = { hooks: {} };\n"),
        },
        // Registry in tests is intentionally non-default (`localhost` mock), not npmjs.
        RegistryLockfileCase {
            name: "non-default-registry-explicit",
            manifest: serde_json::json!({
                "dependencies": {
                    "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
                }
            }),
            npmrc_append: Some("\nstrict-peer-dependencies=false\n"),
            workspace_yaml: None,
            pnpmfile_cjs: None,
        },
    ];

    for case in cases {
        eprintln!("CASE: {}", case.name);
        assert_same_lockfile_for_registry_case(case);
    }
}

#[test]
fn golden_lockfile_suite_matrix_workspace() {
    let cases = vec![("workspace-star", "workspace:*"), ("workspace-caret", "workspace:^")];
    for (case_name, workspace_spec) in cases {
        eprintln!("CASE: {case_name}");
        assert_same_lockfile_for_workspace_manifest(case_name, workspace_spec);
    }
}
