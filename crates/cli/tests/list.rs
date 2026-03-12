use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::{AddMockedRegistry, CommandTempCwd};
use serde_json::Value;
use std::{ffi::OsString, fs, path::Path, process::Command};

fn parse_json_output(output: &[u8]) -> Value {
    serde_json::from_slice(output).expect("parse `pacquet ls --json` output")
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
    let text = String::from_utf8_lossy(output).replace("\r\n", "\n").trim_end().to_string();
    if !text.starts_with("Legend: production dependency, optional only, dev only\n\n") {
        return text;
    }

    let mut normalized_lines = Vec::<String>::new();
    for line in text.lines() {
        if matches!(line, "1 package") || line.ends_with(" packages") {
            continue;
        }
        let line = match line {
            "│" => "",
            other => other,
        };
        let line = line
            .strip_prefix("│   ")
            .or_else(|| line.strip_prefix("├── "))
            .or_else(|| line.strip_prefix("└── "))
            .unwrap_or(line);
        let line =
            if !line.contains(' ') { normalize_tree_package_line(line) } else { line.to_string() };
        normalized_lines.push(line);
    }

    normalized_lines.join("\n").trim_end().to_string()
}

fn normalize_tree_package_line(line: &str) -> String {
    let Some(index) = line.rfind('@') else {
        return line.to_string();
    };
    if index == 0 || index + 1 >= line.len() {
        return line.to_string();
    }
    let version = &line[index + 1..];
    if !version.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        return line.to_string();
    }
    format!("{} {}", &line[..index], version)
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

fn pacquet_command(workspace: &Path, pacquet_bin: &OsString) -> Command {
    Command::new(pacquet_bin).with_current_dir(workspace)
}

#[test]
fn ls_json_long_depth0_no_production_should_match_pnpm_behavior() {
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
            "devDependencies": {
                "@pnpm.e2e/hello-world-js-bin": "1.0.0",
            },
            "optionalDependencies": {
                "@pnpm.e2e/circular-deps-1-of-2": "1.0.2",
            },
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace, &pacquet_bin).with_arg("install").assert().success();

    let assert = pacquet_command(&workspace, &pacquet_bin)
        .with_args(["ls", "--long", "--json", "--depth=0", "--no-production"])
        .assert()
        .success();
    let value = parse_json_output(&assert.get_output().stdout);

    let root_json = value
        .as_array()
        .and_then(|items| items.first())
        .expect("expect one project in list output");
    assert_eq!(
        root_json
            .get("dependencies")
            .and_then(|deps| deps.get("@pnpm.e2e/hello-world-js-bin-parent"))
            .and_then(|dep| dep.get("from"))
            .and_then(Value::as_str),
        Some("@pnpm.e2e/hello-world-js-bin-parent"),
    );
    assert_eq!(
        root_json
            .get("devDependencies")
            .and_then(|deps| deps.get("@pnpm.e2e/hello-world-js-bin"))
            .and_then(|dep| dep.get("from"))
            .and_then(Value::as_str),
        Some("@pnpm.e2e/hello-world-js-bin"),
    );
    assert_eq!(
        root_json
            .get("optionalDependencies")
            .and_then(|deps| deps.get("@pnpm.e2e/circular-deps-1-of-2"))
            .and_then(|dep| dep.get("from"))
            .and_then(Value::as_str),
        Some("@pnpm.e2e/circular-deps-1-of-2"),
    );
    assert!(
        root_json
            .get("devDependencies")
            .and_then(|deps| deps.get("@pnpm.e2e/hello-world-js-bin"))
            .and_then(|dep| dep.get("path"))
            .and_then(Value::as_str)
            .is_some()
    );

    drop((root, mock_instance));
}

#[test]
fn ls_json_long_depth0_should_match_pnpm_alias_shape() {
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
                "hello-alias": "npm:@pnpm.e2e/hello-world-js-bin@1.0.0"
            },
        })
        .to_string(),
    )
    .expect("write package.json");

    pacquet_command(&workspace, &pacquet_bin).with_arg("install").assert().success();

    let assert = pacquet_command(&workspace, &pacquet_bin)
        .with_args(["ls", "--long", "--json", "--depth=0"])
        .assert()
        .success();
    let value = parse_json_output(&assert.get_output().stdout);
    let alias = value
        .as_array()
        .and_then(|items| items.first())
        .and_then(|root| root.get("dependencies"))
        .and_then(|deps| deps.get("hello-alias"))
        .expect("alias should be listed");

    assert_eq!(alias.get("from").and_then(Value::as_str), Some("@pnpm.e2e/hello-world-js-bin"),);
    assert_eq!(alias.get("version").and_then(Value::as_str), Some("1.0.0"));
    assert!(
        alias
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.contains("hello-world-js-bin"))
    );

    drop((root, mock_instance));
}

#[test]
fn ls_json_depth1_should_include_transitive_dependencies() {
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
        .with_args(["ls", "--json", "--depth=1"])
        .assert()
        .success();
    let value = parse_json_output(&assert.get_output().stdout);

    let nested = value
        .as_array()
        .and_then(|items| items.first())
        .and_then(|root| root.get("dependencies"))
        .and_then(|deps| deps.get("@pnpm.e2e/hello-world-js-bin-parent"))
        .and_then(|parent| parent.get("dependencies"))
        .and_then(|deps| deps.get("@pnpm.e2e/hello-world-js-bin"))
        .expect("transitive dependency should be listed at depth=1");

    assert_eq!(nested.get("from").and_then(Value::as_str), Some("@pnpm.e2e/hello-world-js-bin"),);
    assert_eq!(nested.get("version").and_then(Value::as_str), Some("1.0.0"));

    drop((root, mock_instance));
}

#[test]
fn golden_list_suite_matrix_should_match_pnpm_output() {
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
            "optionalDependencies": {
                "@pnpm.e2e/circular-deps-1-of-2": "1.0.2",
            },
        })
        .to_string(),
    )
    .expect("write package.json");

    let cases = [
        ("ls", vec!["--json", "--depth=0"], true),
        ("list", vec!["--long", "--json", "--depth=0", "--no-production"], true),
        ("ls", vec!["--json", "--depth=1"], true),
        ("ls", vec!["--depth=0"], false),
        ("list", vec!["--parseable", "--depth=1"], false),
        ("ls", vec!["--parseable", "--long", "--depth=0"], false),
    ];

    pacquet_command(&workspace, &pacquet_bin).with_arg("install").assert().success();
    let pacquet_outputs = cases
        .iter()
        .map(|(command, flags, is_json)| {
            let mut pacquet_args = vec![*command];
            pacquet_args.extend(flags.iter().copied());
            let output = pacquet_command(&workspace, &pacquet_bin)
                .with_args(pacquet_args)
                .assert()
                .success()
                .get_output()
                .stdout
                .clone();
            ((*command).to_string(), flags.clone(), *is_json, output)
        })
        .collect::<Vec<_>>();

    remove_path_if_exists(&workspace.join("node_modules"));
    remove_path_if_exists(&workspace.join("pnpm-lock.yaml"));
    Command::new(pnpm.get_program())
        .with_current_dir(&workspace)
        .with_args(["install", "--ignore-scripts"])
        .assert()
        .success();

    for (command, flags, is_json, pacquet_output) in pacquet_outputs {
        let mut pnpm_global_args = Vec::<&str>::new();
        let mut pnpm_cmd_args = vec![command.as_str()];
        for flag in flags.iter().copied() {
            if matches!(flag, "--no-production" | "--no-dev" | "--no-optional") {
                pnpm_global_args.push(flag);
            } else {
                pnpm_cmd_args.push(flag);
            }
        }
        let mut pnpm_args = Vec::<&str>::new();
        pnpm_args.extend(pnpm_global_args);
        pnpm_args.extend(pnpm_cmd_args);
        let pnpm_output = Command::new(pnpm.get_program())
            .with_current_dir(&workspace)
            .with_args(pnpm_args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();

        if is_json {
            let pacquet_json = parse_json_output(&pacquet_output);
            let pnpm_json = parse_json_output_with_fallback(&pnpm_output);
            assert_eq!(pacquet_json, pnpm_json, "CASE: {command} {}", flags.join(" "));
        } else {
            let pacquet_text = normalize_text_output(&pacquet_output);
            let pnpm_text = normalize_text_output(&pnpm_output);
            assert_eq!(pacquet_text, pnpm_text, "CASE: {command} {}", flags.join(" "));
        }
    }

    drop((root, mock_instance));
}
