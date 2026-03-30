use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::{AddMockedRegistry, CommandTempCwd};
use std::fs;

const WORKSPACE_STATE_FILE: &str = "node_modules/.pnpm-workspace-state-v1.json";

fn read_workspace_state(workspace: &std::path::Path) -> serde_json::Value {
    let path = workspace.join(WORKSPACE_STATE_FILE);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read workspace state at {}: {e}", path.display()));
    serde_json::from_str(&text).expect("parse workspace state JSON")
}

#[test]
fn single_project_install_creates_workspace_state_file() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "test-project",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet.with_arg("install").assert().success();

    let state_path = workspace.join(WORKSPACE_STATE_FILE);
    assert!(state_path.exists(), "workspace state file should exist after install");

    let state = read_workspace_state(&workspace);
    assert!(state.is_object(), "workspace state should be a JSON object");
    assert!(state.get("lastValidatedTimestamp").is_some(), "should have lastValidatedTimestamp");
    assert!(state.get("projects").is_some(), "should have projects");
    assert!(state.get("settings").is_some(), "should have settings");
    assert!(state.get("pnpmfiles").is_some(), "should have pnpmfiles");
    assert!(state.get("filteredInstall").is_some(), "should have filteredInstall");

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_state_has_empty_projects_for_non_workspace() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "single-project",
            "version": "0.1.0"
        })
        .to_string(),
    )
    .expect("write package.json");

    // No pnpm-workspace.yaml => not a workspace
    pacquet.with_arg("install").assert().success();

    let state = read_workspace_state(&workspace);
    let projects = state.get("projects").and_then(serde_json::Value::as_object);
    assert!(projects.is_some(), "projects should be an object");
    assert!(projects.unwrap().is_empty(), "projects should be empty for a non-workspace install");

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_state_has_correct_settings_fields() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "settings-test",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet.with_arg("install").assert().success();

    let state = read_workspace_state(&workspace);
    let settings = state
        .get("settings")
        .and_then(serde_json::Value::as_object)
        .expect("settings should be a JSON object");

    let expected_keys = [
        "autoInstallPeers",
        "dedupeDirectDeps",
        "dedupeInjectedDeps",
        "dedupePeerDependents",
        "dedupePeers",
        "dev",
        "excludeLinksFromLockfile",
        "hoistPattern",
        "hoistWorkspacePackages",
        "injectWorkspacePackages",
        "linkWorkspacePackages",
        "nodeLinker",
        "optional",
        "peersSuffixMaxLength",
        "preferWorkspacePackages",
        "production",
        "publicHoistPattern",
    ];

    for key in &expected_keys {
        assert!(
            settings.contains_key(*key),
            "settings should contain key '{key}', got keys: {:?}",
            settings.keys().collect::<Vec<_>>()
        );
    }

    // Verify some known default values
    assert_eq!(
        settings.get("nodeLinker").and_then(serde_json::Value::as_str),
        Some("isolated"),
        "default nodeLinker should be 'isolated'"
    );
    assert_eq!(
        settings.get("dedupeDirectDeps").and_then(serde_json::Value::as_bool),
        Some(false),
        "dedupeDirectDeps should default to false"
    );
    assert_eq!(
        settings.get("dedupePeers").and_then(serde_json::Value::as_bool),
        Some(false),
        "dedupePeers should default to false"
    );
    assert_eq!(
        settings.get("preferWorkspacePackages").and_then(serde_json::Value::as_bool),
        Some(false),
        "preferWorkspacePackages should default to false"
    );
    assert_eq!(
        settings.get("dev").and_then(serde_json::Value::as_bool),
        Some(true),
        "dev should default to true (included in default dependency groups)"
    );
    assert_eq!(
        settings.get("optional").and_then(serde_json::Value::as_bool),
        Some(true),
        "optional should default to true (included in default dependency groups)"
    );
    assert_eq!(
        settings.get("production").and_then(serde_json::Value::as_bool),
        Some(true),
        "production should default to true (included in default dependency groups)"
    );

    // Non-workspace install should NOT have workspacePackagePatterns
    assert!(
        !settings.contains_key("workspacePackagePatterns"),
        "non-workspace install should not have workspacePackagePatterns"
    );

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_state_has_valid_last_validated_timestamp() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_millis() as u64;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "timestamp-test",
            "version": "1.0.0"
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet.with_arg("install").assert().success();

    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_millis() as u64;

    let state = read_workspace_state(&workspace);
    let timestamp = state
        .get("lastValidatedTimestamp")
        .and_then(serde_json::Value::as_u64)
        .expect("lastValidatedTimestamp should be a u64");

    assert!(
        timestamp >= before && timestamp <= after,
        "lastValidatedTimestamp ({timestamp}) should be between {before} and {after}"
    );

    drop((root, mock_instance)); // cleanup
}

#[test]
fn workspace_install_populates_projects() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, cache_dir, mock_instance, .. } = npmrc_info;

    // Set up workspace structure
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

    // Create sub-packages
    let app_dir = workspace.join("packages/app");
    let lib_dir = workspace.join("packages/lib");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(&lib_dir).expect("create lib dir");

    fs::write(
        app_dir.join("package.json"),
        serde_json::json!({
            "name": "app",
            "version": "2.0.0"
        })
        .to_string(),
    )
    .expect("write app package.json");

    fs::write(
        app_dir.join(".npmrc"),
        format!(
            "registry={}\nstore-dir={}\ncache-dir={}\n",
            mock_instance.url(),
            store_dir.display(),
            cache_dir.display()
        ),
    )
    .expect("write app .npmrc");

    fs::write(
        lib_dir.join("package.json"),
        serde_json::json!({
            "name": "lib",
            "version": "3.0.0"
        })
        .to_string(),
    )
    .expect("write lib package.json");

    fs::write(
        lib_dir.join(".npmrc"),
        format!(
            "registry={}\nstore-dir={}\ncache-dir={}\n",
            mock_instance.url(),
            store_dir.display(),
            cache_dir.display()
        ),
    )
    .expect("write lib .npmrc");

    pacquet.with_arg("install").assert().success();

    let state = read_workspace_state(&workspace);

    // Verify projects is populated
    let projects = state
        .get("projects")
        .and_then(serde_json::Value::as_object)
        .expect("projects should be a JSON object");
    assert!(!projects.is_empty(), "projects should be populated for a workspace install");

    // Verify the workspace state text contains expected project names
    let state_text =
        fs::read_to_string(workspace.join(WORKSPACE_STATE_FILE)).expect("read workspace state");
    assert!(
        state_text.contains("\"name\": \"app\""),
        "workspace state should contain app project name"
    );
    assert!(
        state_text.contains("\"name\": \"lib\""),
        "workspace state should contain lib project name"
    );

    // Verify settings includes workspacePackagePatterns for workspace installs
    let settings = state
        .get("settings")
        .and_then(serde_json::Value::as_object)
        .expect("settings should be an object");
    assert!(
        settings.contains_key("workspacePackagePatterns"),
        "workspace install should have workspacePackagePatterns in settings"
    );

    // Verify filteredInstall is false for a normal install
    assert_eq!(
        state.get("filteredInstall").and_then(serde_json::Value::as_bool),
        Some(false),
        "filteredInstall should be false for a normal install"
    );

    // Verify pnpmfiles is an array (empty if no .pnpmfile.cjs exists)
    let pnpmfiles = state.get("pnpmfiles").and_then(serde_json::Value::as_array);
    assert!(pnpmfiles.is_some(), "pnpmfiles should be an array");

    drop((root, mock_instance)); // cleanup
}
