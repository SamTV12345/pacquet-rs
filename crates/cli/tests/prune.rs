pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::{AddMockedRegistry, CommandTempCwd};
use std::{
    fs,
    path::Path,
    process::Command,
    sync::{Mutex, OnceLock},
};

fn registry_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().expect("lock registry tests")
}

fn pacquet_command(workspace: &Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn prune_should_remove_orphaned_virtual_store_entries() {
    let _guard = registry_test_lock();
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_arg("install").assert().success();

    let orphan = workspace.join("node_modules/.pnpm/orphan@1.0.0");
    fs::create_dir_all(&orphan).expect("create orphan package dir");
    fs::write(orphan.join("package.json"), "{\"name\":\"orphan\",\"version\":\"1.0.0\"}")
        .expect("write orphan package manifest");

    pacquet_command(&workspace).with_arg("prune").assert().success();

    assert!(!orphan.exists());

    drop((root, mock_instance));
}

#[test]
fn prune_prod_should_remove_dev_dependencies_but_keep_prod_dependencies() {
    let _guard = registry_test_lock();
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0"
            },
            "devDependencies": {
                "@pnpm.e2e/hello-world-js-bin": "1.0.0"
            }
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace).with_arg("install").assert().success();
    pacquet_command(&workspace).with_args(["prune", "--prod"]).assert().success();

    assert!(workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent").exists());
    assert!(!workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists());

    drop((root, mock_instance));
}
