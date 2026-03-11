use crate::state::find_workspace_root;
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_executor::{
    ExecuteCommand, LifecycleScriptOutput, execute_command, execute_command_capture,
};
use pacquet_package_manifest::PackageManifest;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

#[derive(Debug, Args)]
pub struct ExecArgs {
    /// The command to run.
    pub command: String,

    /// Arguments passed to the command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,

    /// Run the command in every workspace package.
    #[clap(short = 'r', long)]
    pub recursive: bool,

    /// Select only matching workspace projects.
    #[clap(long = "filter")]
    pub filter: Vec<String>,

    /// Fail when no workspace projects match the provided filters.
    #[clap(long = "fail-if-no-match")]
    pub fail_if_no_match: bool,

    /// Run commands in parallel for selected workspace projects.
    #[clap(long)]
    pub parallel: bool,

    /// Hide per-package output prefixes when running multiple projects.
    #[clap(long = "reporter-hide-prefix")]
    pub reporter_hide_prefix: bool,

    /// Explicitly keep per-package output prefixes.
    #[clap(long = "no-reporter-hide-prefix")]
    pub no_reporter_hide_prefix: bool,

    /// Limit concurrent workspace command executions.
    #[clap(long = "workspace-concurrency")]
    pub workspace_concurrency: Option<usize>,

    /// Run workspace commands sequentially.
    #[clap(long = "sequential")]
    pub sequential: bool,

    /// Run workspace projects in reverse order.
    #[clap(long = "reverse")]
    pub reverse: bool,

    /// Sort selected workspace projects topologically before execution.
    #[clap(long = "sort")]
    pub sort: bool,

    /// Keep workspace project discovery order without topological sorting.
    #[clap(long = "no-sort")]
    pub no_sort: bool,

    /// Resume execution from the specified package when running recursively.
    #[clap(long = "resume-from")]
    pub resume_from: Option<String>,

    /// Write `pnpm-exec-summary.json` for workspace exec execution.
    #[clap(long = "report-summary")]
    pub report_summary: bool,
}

impl ExecArgs {
    pub fn run(self, dir: PathBuf) -> miette::Result<()> {
        let ExecArgs {
            command,
            args,
            recursive,
            filter,
            fail_if_no_match,
            parallel,
            reporter_hide_prefix: _reporter_hide_prefix,
            no_reporter_hide_prefix,
            workspace_concurrency,
            sequential,
            reverse,
            sort,
            no_sort,
            resume_from,
            report_summary,
        } = self;

        if !recursive && filter.is_empty() && resume_from.is_none() && !report_summary {
            execute_command_in_dir(&dir, &command, &args)?;
            return Ok(());
        }

        let workspace_root_dir = find_workspace_root(&dir).unwrap_or_else(|| dir.clone());
        let graph = WorkspaceGraph::from_workspace_root(&workspace_root_dir);
        if graph.projects.is_empty() {
            execute_command_in_dir(&dir, &command, &args)?;
            return Ok(());
        }

        let mut selected = if filter.is_empty() {
            if recursive {
                (0..graph.projects.len()).collect::<Vec<_>>()
            } else {
                graph
                    .projects
                    .iter()
                    .enumerate()
                    .filter(|(_, project)| project.package_dir == dir)
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>()
            }
        } else {
            resolve_filter_selection(&graph, &dir, &filter)
        };
        selected.sort_by_key(|index| graph.projects[*index].manifest_path.clone());
        selected.dedup();

        if selected.is_empty() && fail_if_no_match {
            miette::bail!("No projects matched the provided filters");
        }
        if selected.is_empty() {
            return Ok(());
        }

        let should_sort = if sort {
            true
        } else if no_sort {
            false
        } else {
            !parallel
        };
        let mut ordered = if should_sort {
            topologically_order_projects(&graph, &selected)
        } else {
            selected.clone()
        };
        if reverse {
            ordered.reverse();
        }
        if let Some(resume_from) = resume_from {
            ordered = resume_from_project(&graph, ordered, &resume_from)?;
        }

        let mut summary = report_summary.then(|| create_execution_status(&graph, &ordered));
        let concurrency = if sequential {
            1
        } else {
            effective_workspace_concurrency(parallel, workspace_concurrency, ordered.len())
        };
        let show_prefix = no_reporter_hide_prefix;
        let capture_output = parallel || show_prefix;

        let mut first_error = None;
        if concurrency <= 1 || ordered.len() <= 1 {
            for index in ordered {
                let project = &graph.projects[index];
                if let Some(summary) = summary.as_mut() {
                    mark_summary_status(summary, project, "running", None, None);
                }
                let started = Instant::now();
                match execute_command_in_project(
                    project,
                    &command,
                    &args,
                    if show_prefix {
                        CommandOutputMode::Capture
                    } else {
                        CommandOutputMode::Inherit
                    },
                ) {
                    Ok(output) => {
                        if show_prefix {
                            print_captured_output(&project.name, "exec", &output, true);
                        }
                        if let Some(summary) = summary.as_mut() {
                            mark_summary_status(
                                summary,
                                project,
                                "passed",
                                Some(started.elapsed().as_secs_f64() * 1_000.0),
                                None,
                            );
                        }
                    }
                    Err(error) => {
                        if let Some(summary) = summary.as_mut() {
                            mark_summary_status(
                                summary,
                                project,
                                "failure",
                                Some(started.elapsed().as_secs_f64() * 1_000.0),
                                Some(error.to_string()),
                            );
                        }
                        first_error = Some(error.to_string());
                        break;
                    }
                }
            }
        } else {
            let run_targets = ordered
                .into_iter()
                .enumerate()
                .map(|(position, index)| ParallelExecTarget {
                    position,
                    key: summary_key(&graph.projects[index]),
                    display_prefix: graph.projects[index].name.clone(),
                    package_dir: graph.projects[index].package_dir.clone(),
                    manifest_path: graph.projects[index].manifest_path.clone(),
                })
                .collect::<Vec<_>>();
            for target in &run_targets {
                if let Some(summary) = summary.as_mut() {
                    mark_summary_status_by_key(summary, &target.key, "running", None, None);
                }
            }
            let outcomes = run_targets_parallel(
                run_targets,
                ParallelExecOptions { command: &command, args: &args, concurrency, capture_output },
            );
            for outcome in outcomes {
                if capture_output {
                    print_captured_output(
                        &outcome.display_prefix,
                        "exec",
                        &LifecycleScriptOutput {
                            stdout: outcome.stdout.clone(),
                            stderr: outcome.stderr.clone(),
                        },
                        show_prefix,
                    );
                }
                if let Some(summary) = summary.as_mut() {
                    mark_summary_status_by_key(
                        summary,
                        &outcome.key,
                        if outcome.error_message.is_some() { "failure" } else { "passed" },
                        Some(outcome.duration_ms),
                        outcome.error_message.clone(),
                    );
                }
                if first_error.is_none() {
                    first_error = outcome.error_message;
                }
            }
        }

        if let Some(summary) = summary.as_ref() {
            write_execution_summary(&dir, summary)?;
        }
        if let Some(error_message) = first_error {
            miette::bail!("{error_message}");
        }
        Ok(())
    }
}

fn execute_command_in_dir(dir: &Path, command: &str, args: &[String]) -> miette::Result<()> {
    let extra_env = command_extra_env(dir)?;
    execute_command(ExecuteCommand {
        pkg_root: dir,
        program: command,
        args,
        extra_env: &extra_env,
    })
    .into_diagnostic()?;
    Ok(())
}

#[derive(Clone, Copy)]
enum CommandOutputMode {
    Inherit,
    Capture,
}

fn execute_command_in_project(
    project: &WorkspaceProject,
    command: &str,
    args: &[String],
    output_mode: CommandOutputMode,
) -> miette::Result<LifecycleScriptOutput> {
    let extra_env = command_extra_env(&project.package_dir)?;
    let opts = ExecuteCommand {
        pkg_root: &project.package_dir,
        program: command,
        args,
        extra_env: &extra_env,
    };
    match output_mode {
        CommandOutputMode::Inherit => {
            execute_command(opts).into_diagnostic()?;
            Ok(LifecycleScriptOutput::default())
        }
        CommandOutputMode::Capture => execute_command_capture(opts).into_diagnostic(),
    }
}

fn command_extra_env(dir: &Path) -> miette::Result<Vec<(OsString, OsString)>> {
    let mut extra_env = vec![
        (OsString::from("npm_config_verify_deps_before_run"), OsString::from("false")),
        (OsString::from("pnpm_config_verify_deps_before_run"), OsString::from("false")),
        (OsString::from("npm_command"), OsString::from("exec")),
    ];
    let manifest_path = dir.join("package.json");
    if manifest_path.is_file() {
        let manifest = PackageManifest::from_path(manifest_path).into_diagnostic()?;
        if let Some(name) = manifest.value().get("name").and_then(serde_json::Value::as_str) {
            extra_env.push((OsString::from("PNPM_PACKAGE_NAME"), OsString::from(name)));
        }
    }
    Ok(extra_env)
}

fn print_captured_output(
    display_prefix: &str,
    command: &str,
    output: &LifecycleScriptOutput,
    show_prefix: bool,
) {
    if show_prefix {
        print_prefixed_lines(&output.stdout, display_prefix, command, false);
        print_prefixed_lines(&output.stderr, display_prefix, command, true);
    } else {
        print!("{}", output.stdout);
        eprint!("{}", output.stderr);
    }
}

fn print_prefixed_lines(text: &str, display_prefix: &str, command: &str, is_stderr: bool) {
    for chunk in text.split_inclusive('\n') {
        let (line, has_newline) =
            chunk.strip_suffix('\n').map_or((chunk, false), |line| (line, true));
        if is_stderr {
            if has_newline {
                eprintln!("{display_prefix} {command}: {line}");
            } else {
                eprint!("{display_prefix} {command}: {line}");
            }
        } else if has_newline {
            println!("{display_prefix} {command}: {line}");
        } else {
            print!("{display_prefix} {command}: {line}");
        }
    }
}

#[derive(Debug)]
struct WorkspaceProject {
    manifest_path: PathBuf,
    package_dir: PathBuf,
    relative_dir: PathBuf,
    name: String,
    dependency_names_all: Vec<String>,
}

#[derive(Debug)]
struct WorkspaceGraph {
    projects: Vec<WorkspaceProject>,
    dependencies_all: Vec<Vec<usize>>,
}

impl WorkspaceGraph {
    fn from_workspace_root(workspace_root: &Path) -> Self {
        let mut projects = collect_package_json_paths(workspace_root)
            .into_iter()
            .filter_map(|manifest_path| {
                let manifest = PackageManifest::from_path(manifest_path.clone()).ok()?;
                let name = manifest.value().get("name")?.as_str()?.to_string();
                let package_dir = manifest_path.parent()?.to_path_buf();
                let relative_dir = package_dir.strip_prefix(workspace_root).ok()?.to_path_buf();
                let dependency_names_all = collect_dependency_names(&manifest);
                Some(WorkspaceProject {
                    manifest_path,
                    package_dir,
                    relative_dir,
                    name,
                    dependency_names_all,
                })
            })
            .collect::<Vec<_>>();
        projects.sort_by_key(|project| project.manifest_path.clone());

        let by_name = projects
            .iter()
            .enumerate()
            .map(|(idx, project)| (project.name.clone(), idx))
            .collect::<HashMap<_, _>>();
        let dependencies_all = projects
            .iter()
            .map(|project| {
                project
                    .dependency_names_all
                    .iter()
                    .filter_map(|name| by_name.get(name).copied())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        Self { projects, dependencies_all }
    }
}

fn collect_package_json_paths(workspace_root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, result: &mut Vec<PathBuf>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_file() && file_name == "package.json" {
                result.push(path);
                continue;
            }
            if file_type.is_dir()
                && !matches!(file_name.as_str(), "node_modules" | ".git" | "target")
            {
                walk(&path, result);
            }
        }
    }

    let mut result = Vec::new();
    walk(workspace_root, &mut result);
    result
}

fn collect_dependency_names(manifest: &PackageManifest) -> Vec<String> {
    let mut names = BTreeSet::new();
    for field in ["dependencies", "optionalDependencies", "peerDependencies", "devDependencies"] {
        if let Some(object) = manifest.value().get(field).and_then(|value| value.as_object()) {
            names.extend(object.keys().cloned());
        }
    }
    names.into_iter().collect()
}

fn resolve_filter_selection(
    graph: &WorkspaceGraph,
    target_dir: &Path,
    filters: &[String],
) -> Vec<usize> {
    let normalized_target = target_dir.to_string_lossy().replace('\\', "/");
    graph
        .projects
        .iter()
        .enumerate()
        .filter(|(_, project)| {
            let relative_dir = project.relative_dir.to_string_lossy().replace('\\', "/");
            filters.iter().any(|selector| {
                let normalized = selector.trim_start_matches("./").replace('\\', "/");
                normalized == project.name
                    || normalized == relative_dir
                    || normalized_target.ends_with(&normalized)
            })
        })
        .map(|(index, _)| index)
        .collect()
}

fn topologically_order_projects(graph: &WorkspaceGraph, selected: &[usize]) -> Vec<usize> {
    fn visit(
        index: usize,
        graph: &WorkspaceGraph,
        selected: &HashSet<usize>,
        temporary: &mut HashSet<usize>,
        permanent: &mut HashSet<usize>,
        ordered: &mut Vec<usize>,
    ) {
        if permanent.contains(&index) || !selected.contains(&index) {
            return;
        }
        if !temporary.insert(index) {
            return;
        }
        for &dependency in &graph.dependencies_all[index] {
            visit(dependency, graph, selected, temporary, permanent, ordered);
        }
        temporary.remove(&index);
        permanent.insert(index);
        ordered.push(index);
    }

    let mut ordered = Vec::with_capacity(selected.len());
    let mut temporary = HashSet::new();
    let mut permanent = HashSet::new();
    let selected_set = selected.iter().copied().collect::<HashSet<_>>();
    let mut manifest_sorted = selected.to_vec();
    manifest_sorted.sort_by_key(|index| graph.projects[*index].manifest_path.clone());
    for index in manifest_sorted {
        visit(index, graph, &selected_set, &mut temporary, &mut permanent, &mut ordered);
    }
    ordered
}

fn resume_from_project(
    graph: &WorkspaceGraph,
    ordered: Vec<usize>,
    resume_from: &str,
) -> miette::Result<Vec<usize>> {
    let position = ordered
        .iter()
        .position(|index| graph.projects[*index].name == resume_from)
        .ok_or_else(|| {
            miette::miette!(
                "Cannot find package {resume_from}. Could not determine where to resume from."
            )
        })?;
    Ok(ordered[position..].to_vec())
}

fn effective_workspace_concurrency(
    parallel: bool,
    workspace_concurrency: Option<usize>,
    runnable_count: usize,
) -> usize {
    let requested = if parallel {
        workspace_concurrency.unwrap_or(usize::MAX)
    } else {
        workspace_concurrency.unwrap_or(1)
    };
    requested.min(runnable_count.max(1))
}

#[derive(Debug, Clone, Serialize)]
struct ExecutionStatusEntry {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug, Serialize)]
struct ExecutionSummaryFile {
    #[serde(rename = "executionStatus")]
    execution_status: BTreeMap<String, ExecutionStatusEntry>,
}

fn create_execution_status(
    graph: &WorkspaceGraph,
    ordered: &[usize],
) -> BTreeMap<String, ExecutionStatusEntry> {
    ordered
        .iter()
        .map(|index| {
            (
                summary_key(&graph.projects[*index]),
                ExecutionStatusEntry {
                    status: "queued".to_string(),
                    duration: None,
                    message: None,
                },
            )
        })
        .collect()
}

fn mark_summary_status(
    summary: &mut BTreeMap<String, ExecutionStatusEntry>,
    project: &WorkspaceProject,
    status: &str,
    duration: Option<f64>,
    message: Option<String>,
) {
    mark_summary_status_by_key(summary, &summary_key(project), status, duration, message);
}

fn mark_summary_status_by_key(
    summary: &mut BTreeMap<String, ExecutionStatusEntry>,
    key: &str,
    status: &str,
    duration: Option<f64>,
    message: Option<String>,
) {
    let entry = summary.entry(key.to_string()).or_insert_with(|| ExecutionStatusEntry {
        status: "queued".to_string(),
        duration: None,
        message: None,
    });
    entry.status = status.to_string();
    entry.duration = duration;
    entry.message = message;
}

fn summary_key(project: &WorkspaceProject) -> String {
    if project.relative_dir.as_os_str().is_empty() {
        ".".to_string()
    } else {
        project.relative_dir.to_string_lossy().replace('\\', "/")
    }
}

fn write_execution_summary(
    dir: &Path,
    execution_status: &BTreeMap<String, ExecutionStatusEntry>,
) -> miette::Result<()> {
    let summary_path = dir.join("pnpm-exec-summary.json");
    let content = serde_json::to_string_pretty(&ExecutionSummaryFile {
        execution_status: execution_status.clone(),
    })
    .into_diagnostic()
    .wrap_err("serialize pnpm-exec-summary.json")?;
    fs::write(&summary_path, content)
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", summary_path.display()))?;
    Ok(())
}

#[derive(Debug, Clone)]
struct ParallelExecTarget {
    position: usize,
    key: String,
    display_prefix: String,
    package_dir: PathBuf,
    manifest_path: PathBuf,
}

#[derive(Debug, Clone)]
struct ParallelExecOutcome {
    position: usize,
    key: String,
    display_prefix: String,
    duration_ms: f64,
    stdout: String,
    stderr: String,
    error_message: Option<String>,
}

#[derive(Clone, Copy)]
struct ParallelExecOptions<'a> {
    command: &'a str,
    args: &'a [String],
    concurrency: usize,
    capture_output: bool,
}

fn run_targets_parallel(
    run_targets: Vec<ParallelExecTarget>,
    opts: ParallelExecOptions<'_>,
) -> Vec<ParallelExecOutcome> {
    let queue =
        Arc::new(Mutex::new(run_targets.into_iter().collect::<VecDeque<ParallelExecTarget>>()));
    let outcomes = Arc::new(Mutex::new(Vec::<ParallelExecOutcome>::new()));
    let should_stop = Arc::new(AtomicBool::new(false));
    let worker_count = opts.concurrency.max(1);
    let mut workers = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let queue = Arc::clone(&queue);
        let outcomes = Arc::clone(&outcomes);
        let should_stop = Arc::clone(&should_stop);
        let command = opts.command.to_string();
        let args = opts.args.to_vec();
        let capture_output = opts.capture_output;
        workers.push(thread::spawn(move || {
            loop {
                if should_stop.load(Ordering::Relaxed) {
                    break;
                }
                let next = {
                    let mut queue = queue.lock().expect("lock parallel queue");
                    queue.pop_front()
                };
                let Some(target) = next else {
                    break;
                };
                let ParallelExecTarget {
                    position,
                    key,
                    display_prefix,
                    package_dir,
                    manifest_path,
                } = target;

                let started = Instant::now();
                let project = WorkspaceProject {
                    manifest_path,
                    package_dir,
                    relative_dir: PathBuf::new(),
                    name: display_prefix.clone(),
                    dependency_names_all: vec![],
                };
                let result = execute_command_in_project(
                    &project,
                    &command,
                    &args,
                    if capture_output {
                        CommandOutputMode::Capture
                    } else {
                        CommandOutputMode::Inherit
                    },
                );
                let duration_ms = started.elapsed().as_secs_f64() * 1_000.0;
                let (stdout, stderr, error_message) = match result {
                    Ok(output) => (output.stdout, output.stderr, None),
                    Err(error) => (String::new(), String::new(), Some(error.to_string())),
                };
                if error_message.is_some() {
                    should_stop.store(true, Ordering::Relaxed);
                }

                let mut outcomes = outcomes.lock().expect("lock parallel outcomes");
                outcomes.push(ParallelExecOutcome {
                    position,
                    key,
                    display_prefix,
                    duration_ms,
                    stdout,
                    stderr,
                    error_message,
                });
            }
        }));
    }

    for worker in workers {
        let _ = worker.join();
    }

    let mut outcomes = Arc::try_unwrap(outcomes)
        .expect("parallel outcomes still referenced")
        .into_inner()
        .expect("extract parallel outcomes");
    outcomes.sort_by_key(|outcome| outcome.position);
    outcomes
}
