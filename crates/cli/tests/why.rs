use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::{AddMockedRegistry, CommandTempCwd};
use serde_json::{Value, json};
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

fn normalize_why_parseable_output(output: &[u8]) -> Vec<String> {
    let mut packages = Vec::<String>::new();
    for line in normalize_text_output(output).lines().filter(|line| !line.is_empty()) {
        if line.contains(" > ") {
            packages.extend(
                line.split(" > ")
                    .skip(1)
                    .filter(|segment| !segment.is_empty())
                    .map(ToOwned::to_owned),
            );
            continue;
        }

        let Some((store_segment, package_name)) = parse_package_id_from_parseable_path(line) else {
            continue;
        };
        packages.push(format!("{package_name}@{store_segment}"));
    }
    packages.sort();
    packages.dedup();
    packages
}

fn parse_package_id_from_parseable_path(path: &str) -> Option<(String, String)> {
    let package_name = path.rsplit("/node_modules/").next()?;
    if package_name == path || package_name.is_empty() {
        return None;
    }

    let store_segment = path.split("/.pnpm/").nth(1)?.split('/').next()?;
    let encoded_name = package_name.replace('/', "+");
    let version = store_segment
        .strip_prefix(&format!("{encoded_name}@"))
        .unwrap_or(store_segment)
        .split('_')
        .next()
        .unwrap_or(store_segment);

    Some((version.to_string(), package_name.to_string()))
}

fn normalize_why_json_output(value: &Value) -> Value {
    let Some(items) = value.as_array() else {
        return value.clone();
    };
    let Some(first) = items.first().and_then(Value::as_object) else {
        return value.clone();
    };

    let mut paths = if first.contains_key("dependents") {
        collect_reverse_why_paths(items)
    } else {
        collect_root_tree_why_paths(items)
    };
    paths.sort();
    json!(paths)
}

fn normalized_string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn collect_reverse_why_paths(items: &[Value]) -> Vec<String> {
    let mut paths = Vec::<String>::new();
    for item in items {
        let Some(item) = item.as_object() else {
            continue;
        };
        let package = format!(
            "{}@{}",
            item.get("name").and_then(Value::as_str).unwrap_or_default(),
            item.get("version").and_then(Value::as_str).unwrap_or_default()
        );
        collect_reverse_dependents(
            item.get("dependents").and_then(Value::as_array).unwrap_or(&Vec::new()),
            &package,
            &mut paths,
        );
    }
    paths
}

fn collect_reverse_dependents(dependents: &[Value], suffix: &str, paths: &mut Vec<String>) {
    for dependent in dependents {
        let Some(dependent) = dependent.as_object() else {
            continue;
        };
        let mut segment = format!(
            "{}@{}",
            dependent.get("name").and_then(Value::as_str).unwrap_or_default(),
            dependent.get("version").and_then(Value::as_str).unwrap_or_default()
        );
        if let Some(dep_field) = dependent.get("depField").and_then(Value::as_str) {
            segment.push_str(&format!(" ({dep_field})"));
        }
        let path = format!("{segment} > {suffix}");
        let nested = dependent.get("dependents").and_then(Value::as_array);
        if let Some(nested) = nested {
            if nested.is_empty() {
                paths.push(path);
            } else {
                collect_reverse_dependents(nested, &path, paths);
            }
        } else {
            paths.push(path);
        }
    }
}

fn collect_root_tree_why_paths(items: &[Value]) -> Vec<String> {
    let mut paths = Vec::<String>::new();
    for item in items {
        let Some(item) = item.as_object() else {
            continue;
        };
        let root = format!(
            "{}@{}",
            item.get("name").and_then(Value::as_str).unwrap_or_default(),
            item.get("version").and_then(Value::as_str).unwrap_or_default()
        );
        for group_name in ["dependencies", "devDependencies", "optionalDependencies"] {
            let Some(group) = item.get(group_name).and_then(Value::as_object) else {
                continue;
            };
            for dependency in group.values() {
                collect_root_tree_dependency_paths(
                    dependency,
                    &format!("{root} ({group_name})"),
                    &mut paths,
                );
            }
        }
    }
    paths
}

fn collect_root_tree_dependency_paths(dependency: &Value, prefix: &str, paths: &mut Vec<String>) {
    let Some(dependency) = dependency.as_object() else {
        return;
    };
    let current = format!(
        "{}@{}",
        dependency.get("from").and_then(Value::as_str).unwrap_or_default(),
        dependency.get("version").and_then(Value::as_str).unwrap_or_default()
    );
    let path = format!("{prefix} > {current}");
    let nested = dependency.get("dependencies").and_then(Value::as_object);
    if let Some(nested) = nested {
        if nested.is_empty() {
            paths.push(path);
        } else {
            for child in nested.values() {
                collect_root_tree_dependency_paths(child, &path, paths);
            }
        }
    } else {
        paths.push(path);
    }
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
fn why_json_should_match_pnpm_dependency_tree_shape() {
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
    let first_result = value.as_array().and_then(|items| items.first()).expect("one project root");
    assert_eq!(first_result.get("name").and_then(Value::as_str), Some("workspace"),);
    let first_dependent = first_result
        .get("dependencies")
        .and_then(|deps| deps.get("@pnpm.e2e/hello-world-js-bin-parent"))
        .expect("package should keep matching dependency path");
    assert_eq!(
        first_dependent.get("from").and_then(Value::as_str),
        Some("@pnpm.e2e/hello-world-js-bin-parent"),
    );
    let nested_match = first_dependent
        .get("dependencies")
        .and_then(|deps| deps.get("@pnpm.e2e/hello-world-js-bin"))
        .expect("matched transitive dependency should be nested under the direct path");
    assert_eq!(
        nested_match.get("from").and_then(Value::as_str),
        Some("@pnpm.e2e/hello-world-js-bin"),
    );

    drop((root, mock_instance));
}

#[test]
fn why_should_match_npm_alias_by_alias_name() {
    let CommandTempCwd { pacquet, root: _root, workspace, npmrc_info, .. } =
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
    let root = value.as_array().and_then(|items| items.first()).expect("one project root");
    let alias = root
        .get("dependencies")
        .and_then(|deps| deps.get("foo"))
        .expect("alias path should be present");
    assert_eq!(
        alias.get("from").and_then(Value::as_str),
        Some("@pnpm.e2e/hello-world-js-bin-parent"),
    );

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

        let depth_zero = args.contains(&"--depth=0");
        let pnpm_output = Command::new(pnpm.get_program())
            .with_current_dir(&workspace)
            .with_args(&args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();

        if is_json {
            let pacquet_json = normalize_why_json_output(&parse_json_output(&pacquet_output));
            let pnpm_json =
                normalize_why_json_output(&parse_json_output_with_fallback(&pnpm_output));
            if depth_zero {
                let pacquet_paths = normalized_string_array(&pacquet_json);
                let pnpm_paths = normalized_string_array(&pnpm_json);
                let pacquet_subset = pacquet_paths.iter().all(|path| pnpm_paths.contains(path));
                let pnpm_subset = pnpm_paths.iter().all(|path| pacquet_paths.contains(path));
                assert!(
                    pacquet_subset || pnpm_subset,
                    "depth=0 why JSON variants diverged too far\npacquet={pacquet_json}\npnpm={pnpm_json}"
                );
            } else {
                assert_eq!(pacquet_json, pnpm_json);
            }
        } else {
            let pacquet_text = normalize_why_parseable_output(&pacquet_output);
            let pnpm_text = normalize_why_parseable_output(&pnpm_output);
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
