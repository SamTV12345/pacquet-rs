use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::process::Command;

fn pacquet_command(workspace: &std::path::Path) -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("pacquet").expect("find pacquet binary").with_current_dir(workspace)
}

#[test]
fn env_use_requires_global_flag() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;

    pacquet_command(&workspace).with_args(["env", "use", "18"]).assert().failure();

    drop((root, mock_instance)); // cleanup
}

#[test]
fn env_help_lists_subcommands() {
    let CommandTempCwd { root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let mock_instance = npmrc_info.mock_instance;

    let assert = pacquet_command(&workspace).with_args(["env", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(stdout.contains("add"));
    assert!(stdout.contains("use"));
    assert!(stdout.contains("remove"));
    assert!(stdout.contains("list"));
    assert!(
        stdout.contains("pacquet env [command] [options] <version> [<additional-versions>...]")
    );
    assert!(stdout.contains("Visit https://pnpm.io/10.x/cli/env"));

    drop((root, mock_instance)); // cleanup
}
