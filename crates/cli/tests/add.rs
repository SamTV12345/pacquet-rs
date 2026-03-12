use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_fs::symlink_dir;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_testing_utils::{
    bin::{AddMockedRegistry, CommandTempCwd},
    fs::{get_all_folders, get_filenames_in_folder, symlink_or_junction_target},
};
use pretty_assertions::assert_eq;
#[cfg(unix)]
use std::fs;
use std::{ffi::OsStr, path::PathBuf};
use tempfile::TempDir;

fn exec_pacquet_in_temp_cwd<Args>(args: Args) -> (TempDir, PathBuf, AddMockedRegistry)
where
    Args: IntoIterator,
    Args::Item: AsRef<OsStr>,
{
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    pacquet.with_args(args).assert().success();
    (root, workspace, npmrc_info)
}

#[test]
fn should_install_all_dependencies() {
    let (root, workspace, anchor) =
        exec_pacquet_in_temp_cwd(["add", "@pnpm.e2e/hello-world-js-bin-parent"]);

    eprintln!("Directory list");
    insta::assert_debug_snapshot!(get_all_folders(&workspace));

    let manifest_path = workspace.join("package.json");

    eprintln!("Ensure the manifest file ({manifest_path:?}) exists");
    assert!(manifest_path.exists());

    let virtual_store_dir = workspace.join("node_modules").join(".pnpm");

    eprintln!("Ensure virtual store dir ({virtual_store_dir:?}) exists");
    assert!(virtual_store_dir.exists());

    eprintln!("Ensure that @pnpm.e2e/hello-world-js-bin has no other dependencies than itself");
    let path = virtual_store_dir.join("@pnpm.e2e+hello-world-js-bin@1.0.0/node_modules");
    assert_eq!(get_filenames_in_folder(&path), ["@pnpm.e2e"]);
    assert_eq!(get_filenames_in_folder(&path.join("@pnpm.e2e")), ["hello-world-js-bin"]);

    eprintln!("Ensure that @pnpm.e2e/hello-world-js-bin-parent has correct dependencies");
    let path = virtual_store_dir.join("@pnpm.e2e+hello-world-js-bin-parent@1.0.0/node_modules");
    assert_eq!(get_filenames_in_folder(&path), ["@pnpm.e2e"]);
    assert_eq!(
        get_filenames_in_folder(&path.join("@pnpm.e2e")),
        ["hello-world-js-bin", "hello-world-js-bin-parent"],
    );

    drop((root, anchor)); // cleanup
}

#[test]
#[cfg(unix)]
pub fn should_symlink_correctly() {
    use pipe_trait::Pipe;

    let (root, workspace, anchor) =
        exec_pacquet_in_temp_cwd(["add", "@pnpm.e2e/hello-world-js-bin-parent"]);

    eprintln!("Directory list");
    insta::assert_debug_snapshot!(get_all_folders(&workspace));

    let manifest_path = workspace.join("package.json");

    eprintln!("Ensure the manifest file ({manifest_path:?}) exists");
    assert!(manifest_path.exists());

    let virtual_store_dir = workspace.join("node_modules").join(".pnpm");

    eprintln!("Ensure virtual store dir ({virtual_store_dir:?}) exists");
    assert!(virtual_store_dir.exists());

    eprintln!("Make sure the symlinks are correct");
    assert_eq!(
        virtual_store_dir
            .join("@pnpm.e2e+hello-world-js-bin-parent@1.0.0")
            .join("node_modules")
            .join("@pnpm.e2e")
            .join("hello-world-js-bin")
            .pipe(fs::canonicalize)
            .expect("canonicalize link"),
        virtual_store_dir
            .join("@pnpm.e2e+hello-world-js-bin@1.0.0")
            .join("node_modules")
            .join("@pnpm.e2e")
            .join("hello-world-js-bin")
            .pipe(fs::canonicalize)
            .expect("canonicalize link target"),
    );

    drop((root, anchor)); // cleanup
}

#[test]
fn should_add_to_package_json() {
    let (root, dir, anchor) = exec_pacquet_in_temp_cwd(["add", "@pnpm.e2e/hello-world-js-bin"]);
    let file = PackageManifest::from_path(dir.join("package.json")).unwrap();
    eprintln!("Ensure @pnpm.e2e/hello-world-js-bin is added to package.json#dependencies");
    assert!(
        file.dependencies([DependencyGroup::Prod])
            .any(|(k, _)| k == "@pnpm.e2e/hello-world-js-bin")
    );
    drop((root, anchor)); // cleanup
}

#[test]
fn scoped_registry_should_override_default_registry_for_add() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { npmrc_path, mock_instance, .. } = npmrc_info;

    std::fs::write(
        &npmrc_path,
        format!(
            "registry=http://127.0.0.1:9/\n@pnpm.e2e:registry={}\nstore-dir=../pacquet-store\ncache-dir=../pacquet-cache\n",
            mock_instance.url()
        ),
    )
    .expect("rewrite .npmrc with scoped registry");

    pacquet.with_args(["add", "@pnpm.e2e/hello-world-js-bin"]).assert().success();

    let file = PackageManifest::from_path(workspace.join("package.json")).unwrap();
    let dependency = file
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "@pnpm.e2e/hello-world-js-bin")
        .map(|(_, version)| version);
    assert_eq!(dependency, Some("^1.0.0"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_add_explicit_version_spec_to_package_json() {
    let (root, dir, anchor) =
        exec_pacquet_in_temp_cwd(["add", "@pnpm.e2e/hello-world-js-bin@1.0.0"]);
    let file = PackageManifest::from_path(dir.join("package.json")).unwrap();
    let dependency = file
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "@pnpm.e2e/hello-world-js-bin")
        .map(|(_, version)| version);
    assert_eq!(dependency, Some("1.0.0"));
    drop((root, anchor)); // cleanup
}

#[test]
fn should_add_explicit_range_spec_to_package_json() {
    let (root, dir, anchor) =
        exec_pacquet_in_temp_cwd(["add", "@pnpm.e2e/hello-world-js-bin@~1.0.0"]);
    let file = PackageManifest::from_path(dir.join("package.json")).unwrap();
    let dependency = file
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "@pnpm.e2e/hello-world-js-bin")
        .map(|(_, version)| version);
    assert_eq!(dependency, Some("~1.0.0"));
    drop((root, anchor)); // cleanup
}

#[test]
fn should_add_latest_tag_spec_to_package_json() {
    let (root, dir, anchor) =
        exec_pacquet_in_temp_cwd(["add", "@pnpm.e2e/hello-world-js-bin@latest"]);
    let file = PackageManifest::from_path(dir.join("package.json")).unwrap();
    let dependency = file
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "@pnpm.e2e/hello-world-js-bin")
        .map(|(_, version)| version);
    assert_eq!(dependency, Some("latest"));
    drop((root, anchor)); // cleanup
}

#[test]
fn should_add_multiple_packages_to_package_json() {
    let (root, dir, anchor) = exec_pacquet_in_temp_cwd([
        "add",
        "@pnpm.e2e/hello-world-js-bin@1.0.0",
        "@pnpm.e2e/hello-world-js-bin-parent@1.0.0",
    ]);
    let file = PackageManifest::from_path(dir.join("package.json")).unwrap();
    let dependencies = file.dependencies([DependencyGroup::Prod]).collect::<Vec<_>>();
    assert!(
        dependencies.iter().any(|(name, version)| {
            *name == "@pnpm.e2e/hello-world-js-bin" && *version == "1.0.0"
        })
    );
    assert!(dependencies.iter().any(|(name, version)| {
        *name == "@pnpm.e2e/hello-world-js-bin-parent" && *version == "1.0.0"
    }));
    drop((root, anchor)); // cleanup
}

#[test]
fn workspace_root_flag_should_add_dependency_to_workspace_root_manifest() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let app_dir = workspace.join("packages/app");
    std::fs::create_dir_all(&app_dir).expect("create workspace package directory");
    std::fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    std::fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace-root",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write workspace root manifest");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");

    pacquet
        .with_args([
            "-C",
            app_dir.to_str().unwrap(),
            "-w",
            "add",
            "@pnpm.e2e/hello-world-js-bin@1.0.0",
        ])
        .assert()
        .success();

    let root_manifest = PackageManifest::from_path(workspace.join("package.json")).unwrap();
    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    assert!(
        root_manifest.dependencies([DependencyGroup::Prod]).any(|(name, version)| {
            name == "@pnpm.e2e/hello-world-js-bin" && version == "1.0.0"
        })
    );
    assert!(
        !app_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, _)| name == "@pnpm.e2e/hello-world-js-bin")
    );

    drop((root, npmrc_info)); // cleanup
}

#[test]
fn add_should_fail_at_workspace_root_without_explicit_root_opt_in() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();

    std::fs::create_dir_all(workspace.join("packages/app")).expect("create workspace package dir");
    std::fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    std::fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace-root",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write workspace root manifest");

    pacquet.with_args(["add", "@pnpm.e2e/hello-world-js-bin@1.0.0"]).assert().failure();

    let root_manifest = PackageManifest::from_path(workspace.join("package.json")).unwrap();
    assert!(
        !root_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, _)| name == "@pnpm.e2e/hello-world-js-bin")
    );

    drop((root, npmrc_info)); // cleanup
}

#[test]
fn ignore_workspace_root_check_should_allow_adding_to_workspace_root() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();

    std::fs::create_dir_all(workspace.join("packages/app")).expect("create workspace package dir");
    std::fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    std::fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace-root",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write workspace root manifest");

    pacquet
        .with_args(["add", "@pnpm.e2e/hello-world-js-bin@1.0.0", "--ignore-workspace-root-check"])
        .assert()
        .success();

    let root_manifest = PackageManifest::from_path(workspace.join("package.json")).unwrap();
    assert!(
        root_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, version)| name == "@pnpm.e2e/hello-world-js-bin" && version == "1.0.0")
    );

    drop((root, npmrc_info)); // cleanup
}

#[test]
fn workspace_flag_should_add_workspace_protocol_dependency() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    std::fs::create_dir_all(&app_dir).expect("create app package directory");
    std::fs::create_dir_all(&lib_dir).expect("create lib package directory");
    std::fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    std::fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "2.3.4"
        })
        .to_string(),
    )
    .expect("write lib manifest");

    pacquet
        .with_args([
            "-C",
            app_dir.to_str().unwrap(),
            "add",
            "@repo/lib",
            "--workspace",
            "--ignore-workspace-root-check",
        ])
        .assert()
        .success();

    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    assert!(
        app_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, spec)| name == "@repo/lib" && spec == "workspace:*")
    );

    drop(root); // cleanup
}

#[test]
fn workspace_flag_should_fail_for_non_workspace_package() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    std::fs::create_dir_all(&app_dir).expect("create app package directory");
    std::fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");

    pacquet
        .with_args([
            "-C",
            app_dir.to_str().unwrap(),
            "add",
            "@repo/missing",
            "--workspace",
            "--ignore-workspace-root-check",
        ])
        .assert()
        .failure();

    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    assert!(
        !app_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, _)| name == "@repo/missing")
    );

    drop(root); // cleanup
}

#[test]
fn workspace_protocol_spec_should_add_workspace_dependency_without_workspace_flag() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    std::fs::create_dir_all(&app_dir).expect("create app package directory");
    std::fs::create_dir_all(&lib_dir).expect("create lib package directory");
    std::fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    std::fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "2.3.4"
        })
        .to_string(),
    )
    .expect("write lib manifest");

    pacquet
        .with_args(["-C", app_dir.to_str().unwrap(), "add", "@repo/lib@workspace:*"])
        .assert()
        .success();

    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    assert!(
        app_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, spec)| name == "@repo/lib" && spec == "workspace:*")
    );
    assert!(app_dir.join("node_modules/@repo/lib").exists());

    drop(root); // cleanup
}

#[test]
fn workspace_protocol_spec_should_fail_for_non_workspace_package() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("packages/app");
    std::fs::create_dir_all(&app_dir).expect("create app package directory");
    std::fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");

    pacquet
        .with_args(["-C", app_dir.to_str().unwrap(), "add", "@repo/missing@workspace:*"])
        .assert()
        .failure();

    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    assert!(
        !app_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, _)| name == "@repo/missing")
    );

    drop(root); // cleanup
}

#[test]
fn add_filter_should_target_only_selected_workspace_project() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    std::fs::create_dir_all(&app_dir).expect("create app package directory");
    std::fs::create_dir_all(&lib_dir).expect("create lib package directory");
    std::fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    std::fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace-root",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write root manifest");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    std::fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write lib manifest");

    pacquet
        .with_args(["add", "@pnpm.e2e/hello-world-js-bin@1.0.0", "--filter", "@repo/app"])
        .assert()
        .success();

    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    let lib_manifest = PackageManifest::from_path(lib_dir.join("package.json")).unwrap();
    assert!(
        app_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, version)| name == "@pnpm.e2e/hello-world-js-bin" && version == "1.0.0")
    );
    assert!(
        !lib_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, _)| name == "@pnpm.e2e/hello-world-js-bin")
    );

    drop((root, mock_instance)); // cleanup
}

#[test]
fn add_filter_should_fail_when_no_workspace_projects_match() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();
    let app_dir = workspace.join("packages/app");
    std::fs::create_dir_all(&app_dir).expect("create app package directory");
    std::fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");

    pacquet
        .with_args(["add", "@pnpm.e2e/hello-world-js-bin@1.0.0", "--filter", "@repo/missing"])
        .assert()
        .failure();

    drop(root); // cleanup
}

#[test]
fn add_recursive_from_subproject_should_target_all_workspace_projects_except_root() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, cache_dir, mock_instance, .. } = npmrc_info;

    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    std::fs::create_dir_all(&app_dir).expect("create app package directory");
    std::fs::create_dir_all(&lib_dir).expect("create lib package directory");
    std::fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
        .expect("write pnpm-workspace.yaml");
    std::fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace-root",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write root manifest");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    std::fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write lib manifest");
    std::fs::write(
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
        .with_args([
            "-C",
            app_dir.to_str().unwrap(),
            "add",
            "@pnpm.e2e/hello-world-js-bin@1.0.0",
            "--recursive",
        ])
        .assert()
        .success();

    let root_manifest = PackageManifest::from_path(workspace.join("package.json")).unwrap();
    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    let lib_manifest = PackageManifest::from_path(lib_dir.join("package.json")).unwrap();
    assert!(
        !root_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, _)| name == "@pnpm.e2e/hello-world-js-bin")
    );
    assert!(
        app_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, version)| name == "@pnpm.e2e/hello-world-js-bin" && version == "1.0.0")
    );
    assert!(
        lib_manifest
            .dependencies([DependencyGroup::Prod])
            .any(|(name, version)| name == "@pnpm.e2e/hello-world-js-bin" && version == "1.0.0")
    );

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_add_local_relative_path_dependency() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("lib");
    std::fs::create_dir_all(&app_dir).expect("create app directory");
    std::fs::create_dir_all(&lib_dir).expect("create lib directory");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    std::fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write lib manifest");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "add", "../lib"]).assert().success();

    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    let dep = app_manifest
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "@repo/lib")
        .map(|(_, spec)| spec.to_string())
        .expect("local dependency spec");
    #[cfg(windows)]
    assert_eq!(dep, r"link:..\lib");
    #[cfg(not(windows))]
    assert_eq!(dep, "link:../lib");

    drop(root); // cleanup
}

#[test]
fn should_add_local_absolute_path_dependency() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("lib");
    std::fs::create_dir_all(&app_dir).expect("create app directory");
    std::fs::create_dir_all(&lib_dir).expect("create lib directory");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    std::fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write lib manifest");

    pacquet
        .with_args(["-C", app_dir.to_str().unwrap(), "add", lib_dir.to_str().unwrap()])
        .assert()
        .success();

    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    let dep = app_manifest
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "@repo/lib")
        .map(|(_, spec)| spec.to_string())
        .expect("local dependency spec");
    assert!(dep.starts_with("link:"));
    assert!(dep.contains("/lib") || dep.ends_with("\\lib"));

    drop(root); // cleanup
}

#[test]
fn should_add_file_protocol_directory_without_package_json() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let pkg_dir = app_dir.join("pkg");
    std::fs::create_dir_all(&pkg_dir).expect("create pkg directory");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    std::fs::write(pkg_dir.join("index.js"), "module.exports = 'pkg';\n").expect("write pkg file");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "add", "file:./pkg"]).assert().success();

    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    let dep = app_manifest
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "pkg")
        .map(|(_, spec)| spec.to_string())
        .expect("local dependency spec");
    assert_eq!(dep, "file:pkg");

    assert!(app_dir.join("node_modules/pkg").exists());
    assert!(app_dir.join("pnpm-lock.yaml").exists());

    drop(root); // cleanup
}

#[test]
fn should_add_file_protocol_local_path_dependency() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("lib");
    std::fs::create_dir_all(&app_dir).expect("create app directory");
    std::fs::create_dir_all(&lib_dir).expect("create lib directory");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    std::fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "@repo/lib",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write lib manifest");
    std::fs::write(lib_dir.join("index.js"), "module.exports = 'lib';\n").expect("write lib file");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "add", "file:../lib"]).assert().success();

    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    let dep = app_manifest
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "@repo/lib")
        .map(|(_, spec)| spec.to_string())
        .expect("local dependency spec");
    #[cfg(windows)]
    assert_eq!(dep, r"file:..\lib");
    #[cfg(not(windows))]
    assert_eq!(dep, "file:../lib");

    assert!(app_dir.join("node_modules/@repo/lib").exists());
    assert!(app_dir.join("pnpm-lock.yaml").exists());

    drop(root); // cleanup
}

#[test]
fn should_add_link_protocol_local_path_dependency_without_current_dir_prefix() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let pkg_dir = app_dir.join("pkg");
    std::fs::create_dir_all(&pkg_dir).expect("create pkg directory");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    std::fs::write(
        pkg_dir.join("package.json"),
        serde_json::json!({
            "name": "pkg",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write pkg manifest");
    std::fs::write(pkg_dir.join("index.js"), "module.exports = 'pkg';\n").expect("write pkg file");

    pacquet.with_args(["-C", app_dir.to_str().unwrap(), "add", "link:./pkg"]).assert().success();

    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    let dep = app_manifest
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "pkg")
        .map(|(_, spec)| spec.to_string())
        .expect("local dependency spec");
    assert_eq!(dep, "link:pkg");

    assert!(app_dir.join("node_modules/pkg").exists());
    let lockfile_content =
        std::fs::read_to_string(app_dir.join("pnpm-lock.yaml")).expect("read lockfile");
    assert!(lockfile_content.contains("specifier: link:pkg"));
    assert!(lockfile_content.contains("version: link:pkg"));

    drop(root); // cleanup
}

#[test]
fn should_add_link_protocol_dependency_with_symlinked_node_modules() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let lib_dir = workspace.join("local-pkg");
    let shared_node_modules = workspace.join("shared-node_modules");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::create_dir_all(&lib_dir).expect("create local-pkg dir");
    std::fs::create_dir_all(&shared_node_modules).expect("create shared node_modules dir");
    symlink_dir(&shared_node_modules, &app_dir.join("node_modules"))
        .expect("symlink app node_modules to shared directory");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    std::fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "local-pkg",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write linked package manifest");
    std::fs::write(lib_dir.join("index.js"), "module.exports = 'linked';\n")
        .expect("write source file");

    pacquet
        .with_args(["-C", app_dir.to_str().unwrap(), "add", "link:../local-pkg"])
        .assert()
        .success();

    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    let dep = app_manifest
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "local-pkg")
        .map(|(_, spec)| spec.to_string())
        .expect("local dependency spec");
    #[cfg(windows)]
    assert_eq!(dep, r"link:..\local-pkg");
    #[cfg(not(windows))]
    assert_eq!(dep, "link:../local-pkg");

    let linked_dep = app_dir.join("node_modules/local-pkg");
    assert!(linked_dep.exists());
    assert_eq!(
        std::fs::read_to_string(linked_dep.join("index.js")).expect("read linked file"),
        "module.exports = 'linked';\n"
    );

    let lockfile_content =
        std::fs::read_to_string(app_dir.join("pnpm-lock.yaml")).expect("read lockfile");
    assert!(lockfile_content.contains("version: link:../local-pkg"));

    drop(root); // cleanup
}

#[test]
fn add_summary_should_format_local_link_like_pnpm() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let local_pkg_dir = workspace.join("local-pkg");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::create_dir_all(&local_pkg_dir).expect("create local-pkg dir");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    std::fs::write(
        local_pkg_dir.join("package.json"),
        serde_json::json!({
            "name": "local-pkg",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write linked package manifest");
    std::fs::write(local_pkg_dir.join("index.js"), "module.exports = 'linked';\n")
        .expect("write source file");

    let assert = pacquet
        .with_args(["-C", app_dir.to_str().unwrap(), "add", "link:../local-pkg"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();

    assert!(stdout.contains("dependencies:"));
    #[cfg(windows)]
    assert!(stdout.contains(r"+ local-pkg <- ..\local-pkg"));
    #[cfg(not(windows))]
    assert!(stdout.contains("+ local-pkg <- ../local-pkg"));

    drop(root); // cleanup
}

#[test]
fn should_preserve_symlink_target_for_link_protocol_dependency() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let app_dir = workspace.join("app");
    let local_pkg_dir = workspace.join("local-pkg");
    let symlink_dir_path = workspace.join("symlink");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::create_dir_all(&local_pkg_dir).expect("create local-pkg dir");
    std::fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write app manifest");
    std::fs::write(
        local_pkg_dir.join("package.json"),
        serde_json::json!({
            "name": "local-pkg",
            "version": "1.0.0",
            "main": "index.js"
        })
        .to_string(),
    )
    .expect("write linked package manifest");
    std::fs::write(local_pkg_dir.join("index.js"), "module.exports = 'linked';\n")
        .expect("write source file");
    symlink_dir(&local_pkg_dir, &symlink_dir_path).expect("create symlink to local package");

    pacquet
        .with_args(["-C", app_dir.to_str().unwrap(), "add", "link:../symlink"])
        .assert()
        .success();

    let app_manifest = PackageManifest::from_path(app_dir.join("package.json")).unwrap();
    let dep = app_manifest
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "local-pkg")
        .map(|(_, spec)| spec.to_string())
        .expect("local dependency spec");
    #[cfg(windows)]
    assert_eq!(dep, r"link:..\symlink");
    #[cfg(not(windows))]
    assert_eq!(dep, "link:../symlink");

    let link_target = symlink_or_junction_target(&app_dir.join("node_modules/local-pkg"))
        .expect("read local dependency link target");
    assert!(link_target.to_string_lossy().contains("symlink"));
    assert!(!link_target.to_string_lossy().contains("local-pkg"));

    drop(root); // cleanup
}

#[test]
fn should_add_npm_alias_dependency() {
    let (root, dir, anchor) =
        exec_pacquet_in_temp_cwd(["add", "hello-alias@npm:@pnpm.e2e/hello-world-js-bin@1.0.0"]);
    let file = PackageManifest::from_path(dir.join("package.json")).unwrap();
    let dependency = file
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "hello-alias")
        .map(|(_, version)| version);
    assert_eq!(dependency, Some("npm:@pnpm.e2e/hello-world-js-bin@1.0.0"));
    drop((root, anchor)); // cleanup
}

#[test]
fn add_summary_should_format_npm_alias_like_pnpm() {
    let CommandTempCwd { pacquet, root, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;

    let assert = pacquet
        .with_args(["add", "hello-alias@npm:@pnpm.e2e/hello-world-js-bin@1.0.0"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();

    assert!(stdout.contains("dependencies:"));
    assert!(stdout.contains("+ hello-alias <- @pnpm.e2e/hello-world-js-bin 1.0.0"));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn add_reporter_silent_should_suppress_output() {
    let CommandTempCwd { pacquet, root, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;

    let assert = pacquet
        .with_args(["add", "@pnpm.e2e/hello-world-js-bin", "--reporter", "silent"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stdout.trim().is_empty());
    assert!(stderr.trim().is_empty());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_add_npm_alias_without_explicit_spec_as_latest_range() {
    let (root, dir, anchor) =
        exec_pacquet_in_temp_cwd(["add", "hello-alias@npm:@pnpm.e2e/hello-world-js-bin"]);
    let file = PackageManifest::from_path(dir.join("package.json")).unwrap();
    let dependency = file
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "hello-alias")
        .map(|(_, version)| version);
    assert_eq!(dependency, Some("npm:@pnpm.e2e/hello-world-js-bin@^1.0.0"));
    drop((root, anchor)); // cleanup
}

#[test]
fn should_add_remote_tarball_dependency() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let tarball_url = format!(
        "{}@pnpm.e2e/hello-world-js-bin/-/hello-world-js-bin-1.0.0.tgz",
        mock_instance.url()
    );

    pacquet.with_args(["add", tarball_url.as_str()]).assert().success();

    let file = PackageManifest::from_path(workspace.join("package.json")).unwrap();
    let dependency = file
        .dependencies([DependencyGroup::Prod])
        .find(|(name, _)| *name == "@pnpm.e2e/hello-world-js-bin")
        .map(|(_, version)| version);
    assert_eq!(dependency, Some(tarball_url.as_str()));
    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists());

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_add_dev_dependency() {
    let (root, dir, anchor) =
        exec_pacquet_in_temp_cwd(["add", "@pnpm.e2e/hello-world-js-bin", "--save-dev"]);
    let file = PackageManifest::from_path(dir.join("package.json")).unwrap();
    eprintln!("Ensure @pnpm.e2e/hello-world-js-bin is added to package.json#devDependencies");
    assert!(
        file.dependencies([DependencyGroup::Dev]).any(|(k, _)| k == "@pnpm.e2e/hello-world-js-bin")
    );
    drop((root, anchor)); // cleanup
}

#[test]
fn should_add_peer_dependency() {
    let (root, dir, anchor) =
        exec_pacquet_in_temp_cwd(["add", "@pnpm.e2e/hello-world-js-bin", "--save-peer"]);
    let file = PackageManifest::from_path(dir.join("package.json")).unwrap();
    eprintln!("Ensure @pnpm.e2e/hello-world-js-bin is added to package.json#devDependencies");
    assert!(
        file.dependencies([DependencyGroup::Dev]).any(|(k, _)| k == "@pnpm.e2e/hello-world-js-bin")
    );
    eprintln!("Ensure @pnpm.e2e/hello-world-js-bin is added to package.json#peerDependencies");
    assert!(
        file.dependencies([DependencyGroup::Peer])
            .any(|(k, _)| k == "@pnpm.e2e/hello-world-js-bin")
    );
    drop((root, anchor)); // cleanup
}
