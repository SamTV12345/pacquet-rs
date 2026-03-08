use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::{AddMockedRegistry, CommandTempCwd};
use serde_json::Value;
use std::{ffi::OsString, fs, path::Path, process::Command};

fn parse_json_output(output: &[u8]) -> Value {
    serde_json::from_slice(output).expect("parse `pacquet why --json` output")
}

fn parse_json_output_with_fallback(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .or_else(|_| {
            let start = output
                .iter()
                .position(|byte| *byte == b'[' || *byte == b'{')
                .expect("find start of JSON payload");
            serde_json::from_slice(&output[start..])
        })
        .expect("parse JSON output")
}

fn normalize_text_output(output: &[u8]) -> String {
    String::from_utf8_lossy(output).replace("\r\n", "\n").trim_end().to_string()
}

fn pacquet_command(workspace: &Path, pacquet_bin: &OsString) -> Command {
    Command::new(pacquet_bin).with_current_dir(workspace)
}

#[test]
fn why_should_fail_without_package_name() {
    let CommandTempCwd { pacquet, root, workspace: _, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    pacquet.with_arg("why").assert().failure();

    drop((root, mock_instance));
}

#[test]
fn why_json_should_show_reverse_dependency_chain() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
            },
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace, &pacquet_bin).with_arg("install").assert().success();

    let assert = pacquet_command(&workspace, &pacquet_bin)
        .with_args(["why", "@pnpm.e2e/hello-world-js-bin", "--json"])
        .assert()
        .success();
    let value = parse_json_output(&assert.get_output().stdout);
    let first_result =
        value.as_array().and_then(|items| items.first()).expect("at least one match");
    assert_eq!(
        first_result.get("name").and_then(Value::as_str),
        Some("@pnpm.e2e/hello-world-js-bin"),
    );
    let first_dependent = first_result
        .get("dependents")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .expect("package should have dependents");
    assert_eq!(
        first_dependent.get("name").and_then(Value::as_str),
        Some("@pnpm.e2e/hello-world-js-bin-parent"),
    );
    let importer_leaf = first_dependent
        .get("dependents")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .expect("transitive importer leaf should be present");
    assert_eq!(importer_leaf.get("depField").and_then(Value::as_str), Some("dependencies"),);

    drop((root, mock_instance));
}

#[test]
fn why_should_match_npm_alias_by_alias_name() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "dependencies": {
                "foo": "npm:@pnpm.e2e/hello-world-js-bin-parent@1.0.0",
            },
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace, &pacquet_bin).with_arg("install").assert().success();

    let assert = pacquet_command(&workspace, &pacquet_bin)
        .with_args(["why", "foo", "--json"])
        .assert()
        .success();
    let value = parse_json_output(&assert.get_output().stdout);
    let contains_alias_target = value.as_array().into_iter().flatten().any(|item| {
        item.get("name").and_then(Value::as_str) == Some("@pnpm.e2e/hello-world-js-bin-parent")
    });
    assert!(contains_alias_target);

    drop((root, mock_instance));
}

#[test]
fn golden_why_suite_matrix_should_match_pnpm_output() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();
    if std::process::Command::new(pnpm.get_program()).arg("--version").output().is_err() {
        drop((root, mock_instance));
        return;
    }

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "workspace",
            "version": "1.0.0",
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
            },
            "devDependencies": {
                "@pnpm.e2e/hello-world-js-bin": "1.0.0",
            },
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace, &pacquet_bin).with_arg("install").assert().success();

    let cases = vec![
        (vec!["why", "@pnpm.e2e/hello-world-js-bin", "--json"], true),
        (vec!["why", "@pnpm.e2e/hello-world-js-bin", "--json", "--depth=0"], true),
        (vec!["why", "@pnpm.e2e/hello-world-js-bin", "--parseable"], false),
    ];

    for (args, is_json) in cases {
        let pacquet_output = pacquet_command(&workspace, &pacquet_bin)
            .with_args(args.clone())
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();

        let pnpm_output = Command::new(pnpm.get_program())
            .with_current_dir(&workspace)
            .with_args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();

        if is_json {
            let pacquet_json = parse_json_output(&pacquet_output);
            let pnpm_json = parse_json_output_with_fallback(&pnpm_output);
            assert_eq!(pacquet_json, pnpm_json);
        } else {
            let pacquet_text = normalize_text_output(&pacquet_output);
            let pnpm_text = normalize_text_output(&pnpm_output);
            assert_eq!(pacquet_text, pnpm_text);
        }
    }

    drop((root, mock_instance));
}

#[test]
fn why_should_ignore_unreachable_workspace_importers() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;
    let pacquet_bin = pacquet.get_program().to_os_string();

    fs::write(workspace.join("pnpm-workspace.yaml"), "packages:\n  - src\n  - bin\n")
        .expect("write pnpm-workspace.yaml");
    fs::create_dir_all(workspace.join("src")).expect("create src");
    fs::create_dir_all(workspace.join("bin")).expect("create bin");

    fs::write(
        workspace.join("package.json"),
        serde_json::json!({
            "name": "root",
            "version": "1.0.0",
            "dependencies": {
                "ep_etherpad-lite": "link:src",
            },
        })
        .to_string(),
    )
    .expect("write root package.json");
    fs::write(
        workspace.join("src").join("package.json"),
        serde_json::json!({
            "name": "ep_etherpad-lite",
            "version": "2.6.1",
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
            },
        })
        .to_string(),
    )
    .expect("write src package.json");
    fs::write(
        workspace.join("bin").join("package.json"),
        serde_json::json!({
            "name": "bin",
            "version": "2.6.1",
            "dependencies": {
                "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
            },
        })
        .to_string(),
    )
    .expect("write bin package.json");

    pacquet_command(&workspace, &pacquet_bin).with_arg("install").assert().success();

    let output = pacquet_command(&workspace, &pacquet_bin)
        .with_args(["why", "@pnpm.e2e/hello-world-js-bin-parent"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = normalize_text_output(&output);
    assert!(output.contains("ep_etherpad-lite@2.6.1 (dependencies)"));
    assert!(!output.contains("bin@2.6.1 (dependencies)"));

    drop((root, mock_instance));
}
