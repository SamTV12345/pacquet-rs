use clap::Args;
use glob::Pattern;
use miette::{Context, IntoDiagnostic};
use pacquet_executor::{
    ExecuteLifecycleScript, LifecycleScriptOutput, execute_lifecycle_script,
    execute_lifecycle_script_capture,
};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::PackageManifest;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    env as std_env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

#[derive(Debug, Args)]
pub struct RunArgs {
    /// A pre-defined package script.
    pub command: String,

    /// Any additional arguments passed after the script name.
    pub args: Vec<String>,

    /// Avoid exiting with a non-zero exit code when the script is undefined.
    #[clap(long)]
    pub if_present: bool,

    /// Run the script in every workspace package.
    #[clap(short = 'r', long)]
    pub recursive: bool,

    /// Select only matching workspace projects.
    #[clap(long = "filter")]
    pub filter: Vec<String>,

    /// Select workspace projects with production-only dependency traversal semantics.
    #[clap(long = "filter-prod")]
    pub filter_prod: Vec<String>,

    /// Fail when no workspace projects match the provided filters.
    #[clap(long = "fail-if-no-match")]
    pub fail_if_no_match: bool,

    /// Run scripts in parallel for selected workspace projects.
    #[clap(long)]
    pub parallel: bool,

    /// Hide per-package output prefixes when running multiple projects.
    #[clap(long = "reporter-hide-prefix")]
    pub reporter_hide_prefix: bool,

    /// Explicitly keep per-package output prefixes.
    #[clap(long = "no-reporter-hide-prefix")]
    pub no_reporter_hide_prefix: bool,

    /// Group and print project output after script completion.
    #[clap(long = "aggregate-output")]
    pub aggregate_output: bool,

    /// Run workspace projects in reverse order.
    #[clap(long = "reverse")]
    pub reverse: bool,

    /// Limit concurrent workspace script executions.
    #[clap(long = "workspace-concurrency")]
    pub workspace_concurrency: Option<usize>,

    /// Run workspace scripts sequentially.
    #[clap(long = "sequential")]
    pub sequential: bool,

    /// Stream output continuously (accepted for compatibility).
    #[clap(long = "stream")]
    pub stream: bool,

    /// Continue executing remaining workspace scripts even when one fails.
    #[clap(long = "no-bail")]
    pub no_bail: bool,

    /// Stop on first workspace script failure.
    #[clap(long = "bail")]
    pub bail: bool,

    /// Sort selected workspace projects topologically before execution.
    #[clap(long = "sort")]
    pub sort: bool,

    /// Keep workspace project discovery order without topological sorting.
    #[clap(long = "no-sort")]
    pub no_sort: bool,

    /// Resume execution from the specified package when running recursively.
    #[clap(long = "resume-from")]
    pub resume_from: Option<String>,

    /// Write `pnpm-exec-summary.json` for workspace run execution.
    #[clap(long = "report-summary")]
    pub report_summary: bool,
}

impl RunArgs {
    /// Execute the subcommand.
    pub fn run(self, manifest_path: PathBuf, config: &Npmrc) -> miette::Result<()> {
        let RunArgs {
            command,
            args,
            if_present,
            recursive,
            filter,
            filter_prod,
            fail_if_no_match,
            parallel,
            reporter_hide_prefix,
            no_reporter_hide_prefix,
            aggregate_output,
            reverse,
            workspace_concurrency,
            sequential,
            stream,
            no_bail,
            bail,
            sort,
            no_sort,
            resume_from,
            report_summary,
        } = self;
        let reporter_hide_prefix = if reporter_hide_prefix {
            Some(true)
        } else if no_reporter_hide_prefix {
            Some(false)
        } else {
            None
        };
        let sort = if sort {
            Some(true)
        } else if no_sort {
            Some(false)
        } else {
            None
        };
        let workspace_concurrency = if sequential { Some(1) } else { workspace_concurrency };
        if !recursive
            && filter.is_empty()
            && filter_prod.is_empty()
            && !fail_if_no_match
            && !parallel
            && reporter_hide_prefix.is_none()
            && !aggregate_output
            && !reverse
            && workspace_concurrency.is_none()
            && !sequential
            && !stream
            && !no_bail
            && !bail
            && sort.is_none()
            && resume_from.is_none()
            && !report_summary
        {
            return run_named_script(manifest_path, &command, &args, if_present, false, config);
        }

        let package_dir =
            manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        execute_embedded_pnpm_run(
            EmbeddedPnpmRun {
                dir: None,
                recursive,
                workspace_root: false,
                parallel,
                workspace_concurrency,
                reverse,
                sort,
                resume_from,
                report_summary,
                reporter_hide_prefix,
                aggregate_output,
                use_stderr: false,
                filters: filter
                    .into_iter()
                    .map(|selector| FilterSelector { selector, prod_only: false })
                    .chain(
                        filter_prod
                            .into_iter()
                            .map(|selector| FilterSelector { selector, prod_only: true }),
                    )
                    .collect(),
                fail_if_no_match,
                changed_files_ignore_patterns: Vec::new(),
                test_patterns: Vec::new(),
                no_bail,
                if_present,
                command,
                args,
            },
            &package_dir,
            &[],
            config,
        )
    }
}

pub fn run_test(manifest_path: PathBuf, config: &Npmrc) -> miette::Result<()> {
    run_named_script(manifest_path, "test", &[], false, false, config)
}

pub fn run_start(manifest_path: PathBuf, config: &Npmrc) -> miette::Result<()> {
    run_named_script(manifest_path, "start", &[], false, true, config)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptOutputMode {
    Inherit,
    Capture,
}

#[derive(Clone, Copy)]
struct ExecuteScriptContext<'a> {
    manifest_path: &'a Path,
    package_dir: &'a Path,
    init_cwd: &'a Path,
    config: &'a Npmrc,
}

pub fn run_named_script(
    manifest_path: PathBuf,
    script_name: &str,
    passed_thru_args: &[String],
    if_present: bool,
    start_fallback: bool,
    config: &Npmrc,
) -> miette::Result<()> {
    run_named_script_with_output_mode(
        manifest_path,
        script_name,
        passed_thru_args,
        if_present,
        start_fallback,
        config,
        ScriptOutputMode::Inherit,
    )
    .map(|_| ())
}

fn run_named_script_with_output_mode(
    manifest_path: PathBuf,
    script_name: &str,
    passed_thru_args: &[String],
    if_present: bool,
    start_fallback: bool,
    config: &Npmrc,
    output_mode: ScriptOutputMode,
) -> miette::Result<LifecycleScriptOutput> {
    let manifest = PackageManifest::from_path(manifest_path.clone())
        .wrap_err("getting the package.json in current directory")?;
    let package_dir =
        manifest_path.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let init_cwd = std_env::current_dir().into_diagnostic().wrap_err("get current directory")?;
    let ctx = ExecuteScriptContext {
        manifest_path: &manifest_path,
        package_dir: &package_dir,
        init_cwd: &init_cwd,
        config,
    };
    let mut captured_output = LifecycleScriptOutput::default();

    let script =
        resolve_script_text(&manifest, &package_dir, script_name, if_present, start_fallback)?;
    let Some(script) = script else {
        return Ok(captured_output);
    };

    if config.enable_pre_post_scripts {
        let pre_name = format!("pre{script_name}");
        if !script.contains(&pre_name)
            && let Some(pre_script) = manifest.script(&pre_name, true)?
        {
            let output =
                execute_script_with_output_mode(ctx, &pre_name, pre_script, &[], output_mode)?;
            captured_output = merge_output(captured_output, output);
        }
    }

    let output =
        execute_script_with_output_mode(ctx, script_name, &script, passed_thru_args, output_mode)?;
    captured_output = merge_output(captured_output, output);

    if config.enable_pre_post_scripts {
        let post_name = format!("post{script_name}");
        if !script.contains(&post_name)
            && let Some(post_script) = manifest.script(&post_name, true)?
        {
            let output =
                execute_script_with_output_mode(ctx, &post_name, post_script, &[], output_mode)?;
            captured_output = merge_output(captured_output, output);
        }
    }

    Ok(captured_output)
}

fn merge_output(
    mut lhs: LifecycleScriptOutput,
    rhs: LifecycleScriptOutput,
) -> LifecycleScriptOutput {
    lhs.stdout.push_str(&rhs.stdout);
    lhs.stderr.push_str(&rhs.stderr);
    lhs
}

fn resolve_script_text(
    manifest: &PackageManifest,
    package_dir: &Path,
    script_name: &str,
    if_present: bool,
    start_fallback: bool,
) -> miette::Result<Option<String>> {
    if let Some(script) = manifest.script(script_name, true)? {
        return Ok(Some(script.to_string()));
    }

    if start_fallback && script_name == "start" {
        let server_js = package_dir.join("server.js");
        if server_js.is_file() {
            return Ok(Some("node server.js".to_string()));
        }
        miette::bail!("Missing script start or file server.js");
    }

    if if_present {
        Ok(None)
    } else {
        manifest
            .script(script_name, false)
            .map(|value| value.map(str::to_string))
            .map_err(Into::into)
    }
}

fn execute_script_with_output_mode(
    ctx: ExecuteScriptContext<'_>,
    script_name: &str,
    script: &str,
    args: &[String],
    output_mode: ScriptOutputMode,
) -> miette::Result<LifecycleScriptOutput> {
    if let Some(embedded) = parse_embedded_pnpm_run(script) {
        execute_embedded_pnpm_run(embedded, ctx.package_dir, args, ctx.config)?;
        return Ok(LifecycleScriptOutput::default());
    }

    let opts = ExecuteLifecycleScript {
        pkg_root: ctx.package_dir,
        package_json_path: ctx.manifest_path,
        script_name,
        script,
        args,
        script_shell: ctx.config.script_shell.as_deref(),
        shell_emulator: ctx.config.shell_emulator,
        init_cwd: ctx.init_cwd,
    };
    let output = match output_mode {
        ScriptOutputMode::Inherit => {
            execute_lifecycle_script(opts)
                .wrap_err_with(|| format!("executing script `{script_name}`"))?;
            LifecycleScriptOutput::default()
        }
        ScriptOutputMode::Capture => execute_lifecycle_script_capture(opts)
            .wrap_err_with(|| format!("executing script `{script_name}`"))?,
    };
    Ok(output)
}

#[derive(Debug)]
struct EmbeddedPnpmRun {
    dir: Option<PathBuf>,
    recursive: bool,
    workspace_root: bool,
    parallel: bool,
    workspace_concurrency: Option<usize>,
    reverse: bool,
    sort: Option<bool>,
    resume_from: Option<String>,
    report_summary: bool,
    reporter_hide_prefix: Option<bool>,
    aggregate_output: bool,
    use_stderr: bool,
    filters: Vec<FilterSelector>,
    fail_if_no_match: bool,
    changed_files_ignore_patterns: Vec<String>,
    test_patterns: Vec<String>,
    no_bail: bool,
    if_present: bool,
    command: String,
    args: Vec<String>,
}

#[derive(Debug, Clone)]
struct FilterSelector {
    selector: String,
    prod_only: bool,
}

fn parse_embedded_pnpm_run(script: &str) -> Option<EmbeddedPnpmRun> {
    let tokens = shlex::split(script)?;
    if tokens.is_empty() || !is_pnpm_command_token(&tokens[0]) {
        return None;
    }

    let mut dir = None;
    let mut recursive = false;
    let mut workspace_root = false;
    let mut parallel = false;
    let mut workspace_concurrency = None;
    let mut reverse = false;
    let mut sort = None;
    let mut resume_from = None;
    let mut report_summary = false;
    let mut reporter_hide_prefix = None;
    let mut aggregate_output = false;
    let mut use_stderr = false;
    let mut filters = Vec::new();
    let mut fail_if_no_match = false;
    let mut changed_files_ignore_patterns = Vec::new();
    let mut test_patterns = Vec::new();
    let mut no_bail = false;
    let mut if_present = false;
    let mut idx = 1usize;

    while idx < tokens.len() {
        let token = &tokens[idx];
        if token == "run" || token == "run-script" {
            idx += 1;
            break;
        }

        if token == "--filter" {
            idx += 1;
            filters.push(FilterSelector { selector: tokens.get(idx)?.clone(), prod_only: false });
            idx += 1;
            continue;
        }
        if let Some(value) = token.strip_prefix("--filter=") {
            filters.push(FilterSelector { selector: value.to_string(), prod_only: false });
            idx += 1;
            continue;
        }
        if token == "--filter-prod" {
            idx += 1;
            filters.push(FilterSelector { selector: tokens.get(idx)?.clone(), prod_only: true });
            idx += 1;
            continue;
        }
        if let Some(value) = token.strip_prefix("--filter-prod=") {
            filters.push(FilterSelector { selector: value.to_string(), prod_only: true });
            idx += 1;
            continue;
        }
        if token == "--changed-files-ignore-pattern" {
            idx += 1;
            changed_files_ignore_patterns.push(tokens.get(idx)?.clone());
            idx += 1;
            continue;
        }
        if let Some(value) = token.strip_prefix("--changed-files-ignore-pattern=") {
            changed_files_ignore_patterns.push(value.to_string());
            idx += 1;
            continue;
        }
        if token == "--test-pattern" {
            idx += 1;
            test_patterns.push(tokens.get(idx)?.clone());
            idx += 1;
            continue;
        }
        if let Some(value) = token.strip_prefix("--test-pattern=") {
            test_patterns.push(value.to_string());
            idx += 1;
            continue;
        }
        if token == "--fail-if-no-match" {
            fail_if_no_match = true;
            idx += 1;
            continue;
        }
        if token == "-C" || token == "--dir" {
            idx += 1;
            dir = Some(PathBuf::from(tokens.get(idx)?));
            idx += 1;
            continue;
        }
        if let Some(value) = token.strip_prefix("--dir=") {
            dir = Some(PathBuf::from(value));
            idx += 1;
            continue;
        }
        if token == "--if-present" {
            if_present = true;
            idx += 1;
            continue;
        }
        if token == "-r" || token == "--recursive" {
            recursive = true;
            idx += 1;
            continue;
        }
        if token == "-w" || token == "--workspace-root" {
            workspace_root = true;
            idx += 1;
            continue;
        }
        if token == "--parallel" {
            parallel = true;
            idx += 1;
            continue;
        }
        if token == "--sequential" {
            workspace_concurrency = Some(1);
            idx += 1;
            continue;
        }
        if token == "--reverse" {
            reverse = true;
            idx += 1;
            continue;
        }
        if token == "--sort" {
            sort = Some(true);
            idx += 1;
            continue;
        }
        if token == "--no-sort" {
            sort = Some(false);
            idx += 1;
            continue;
        }
        if token == "--resume-from" {
            idx += 1;
            resume_from = Some(tokens.get(idx)?.clone());
            idx += 1;
            continue;
        }
        if let Some(value) = token.strip_prefix("--resume-from=") {
            resume_from = Some(value.to_string());
            idx += 1;
            continue;
        }
        if token == "--report-summary" {
            report_summary = true;
            idx += 1;
            continue;
        }
        if token == "--reporter-hide-prefix" {
            reporter_hide_prefix = Some(true);
            idx += 1;
            continue;
        }
        if token == "--no-reporter-hide-prefix" {
            reporter_hide_prefix = Some(false);
            idx += 1;
            continue;
        }
        if token == "--aggregate-output" {
            aggregate_output = true;
            idx += 1;
            continue;
        }
        if token == "--use-stderr" {
            use_stderr = true;
            idx += 1;
            continue;
        }
        if token == "--workspace-concurrency" {
            idx += 1;
            workspace_concurrency = Some(parse_workspace_concurrency(tokens.get(idx)?)?);
            idx += 1;
            continue;
        }
        if let Some(value) = token.strip_prefix("--workspace-concurrency=") {
            workspace_concurrency = Some(parse_workspace_concurrency(value)?);
            idx += 1;
            continue;
        }
        if token == "--no-bail" {
            no_bail = true;
            idx += 1;
            continue;
        }
        if token == "--bail" {
            no_bail = false;
            idx += 1;
            continue;
        }

        if matches!(token.as_str(), "--stream" | "--color" | "--no-color") {
            idx += 1;
            continue;
        }
        if matches!(token.as_str(), "--loglevel") {
            idx += 1;
            let _ = tokens.get(idx)?;
            idx += 1;
            continue;
        }
        if token.starts_with("--loglevel=") {
            idx += 1;
            continue;
        }

        return None;
    }

    let command = tokens.get(idx)?.clone();
    idx += 1;
    let mut args = tokens[idx..].to_vec();
    if args.first().is_some_and(|arg| arg == "--") {
        args.remove(0);
    }

    Some(EmbeddedPnpmRun {
        dir,
        recursive,
        workspace_root,
        parallel,
        workspace_concurrency,
        reverse,
        sort,
        resume_from,
        report_summary,
        reporter_hide_prefix,
        aggregate_output,
        use_stderr,
        filters,
        fail_if_no_match,
        changed_files_ignore_patterns,
        test_patterns,
        no_bail,
        if_present,
        command,
        args,
    })
}

fn is_pnpm_command_token(token: &str) -> bool {
    let command = token.rsplit(['/', '\\']).next().unwrap_or(token).to_ascii_lowercase();
    matches!(command.as_str(), "pnpm" | "pnpm.cmd" | "pnpm.exe" | "pnpm.ps1")
}

fn execute_embedded_pnpm_run(
    embedded: EmbeddedPnpmRun,
    package_dir: &Path,
    outer_args: &[String],
    config: &Npmrc,
) -> miette::Result<()> {
    let EmbeddedPnpmRun {
        dir,
        recursive,
        workspace_root: include_workspace_root,
        parallel,
        workspace_concurrency,
        reverse,
        sort,
        resume_from,
        report_summary,
        reporter_hide_prefix,
        aggregate_output,
        use_stderr,
        filters,
        fail_if_no_match,
        changed_files_ignore_patterns,
        test_patterns,
        no_bail,
        if_present,
        command,
        mut args,
    } = embedded;
    args.extend_from_slice(outer_args);

    let target_dir = match dir {
        Some(dir) if dir.is_absolute() => dir,
        Some(dir) => package_dir.join(dir),
        None => package_dir.to_path_buf(),
    };

    if !recursive
        && !include_workspace_root
        && filters.is_empty()
        && resume_from.is_none()
        && !report_summary
    {
        return run_named_script(
            target_dir.join("package.json"),
            &command,
            &args,
            if_present,
            false,
            config,
        );
    }

    let workspace_root_dir = find_workspace_root(&target_dir).unwrap_or_else(|| target_dir.clone());
    let graph = WorkspaceGraph::from_workspace_root(&workspace_root_dir);
    if graph.projects.is_empty() {
        return run_named_script(
            target_dir.join("package.json"),
            &command,
            &args,
            if_present,
            false,
            config,
        );
    }
    let root_project =
        graph.projects.iter().enumerate().find_map(|(index, project)| {
            (project.package_dir == workspace_root_dir).then_some(index)
        });

    let mut selected = if filters.is_empty() {
        if recursive {
            let mut all = (0..graph.projects.len()).collect::<Vec<_>>();
            if !include_workspace_root && let Some(root_index) = root_project {
                all.retain(|index| *index != root_index);
            }
            all
        } else if include_workspace_root {
            root_project.map_or_else(Vec::new, |index| vec![index])
        } else {
            graph
                .projects
                .iter()
                .enumerate()
                .filter(|(_, project)| project.package_dir == target_dir)
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        }
    } else {
        resolve_filter_selection(
            &graph,
            &target_dir,
            &filters,
            &changed_files_ignore_patterns,
            &test_patterns,
        )
    };
    if include_workspace_root
        && !recursive
        && !filters.is_empty()
        && let Some(root_index) = root_project
        && !selected.contains(&root_index)
    {
        selected.push(root_index);
    }
    selected.sort_by_key(|index| graph.projects[*index].manifest_path.clone());
    selected.dedup();

    if selected.is_empty() && fail_if_no_match {
        miette::bail!("No projects matched the provided filters");
    }
    if selected.is_empty() {
        return Ok(());
    }

    let should_sort = sort.unwrap_or(!parallel);
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
    let mut runnable = Vec::new();
    for &index in &ordered {
        let project = &graph.projects[index];
        let manifest =
            PackageManifest::from_path(project.manifest_path.clone()).wrap_err_with(|| {
                format!("load package manifest: {}", project.manifest_path.display())
            })?;
        if manifest.script(&command, true)?.is_some() {
            runnable.push(index);
        } else if let Some(summary) = summary.as_mut() {
            mark_summary_status(summary, project, "skipped", None, None);
        }
    }
    if runnable.is_empty() {
        if let Some(summary) = summary.as_ref() {
            write_execution_summary(&target_dir, summary)?;
        }
        if !if_present && command != "test" {
            miette::bail!("None of the selected packages has a \"{command}\" script");
        }
        return Ok(());
    }

    let concurrency =
        effective_workspace_concurrency(parallel, workspace_concurrency, runnable.len());
    let show_prefix = !reporter_hide_prefix.unwrap_or(false) && runnable.len() > 1;
    let capture_parallel_output = show_prefix || aggregate_output;
    let mut first_error = None;
    if concurrency <= 1 || runnable.len() <= 1 {
        for index in runnable {
            let manifest_path = graph.projects[index].manifest_path.clone();
            let project = &graph.projects[index];
            if let Some(summary) = summary.as_mut() {
                mark_summary_status(summary, project, "running", None, None);
            }
            let started = Instant::now();
            match run_named_script(manifest_path, &command, &args, if_present, false, config) {
                Ok(()) => {
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
                    if no_bail {
                        eprintln!("Script execution failed but --no-bail was set: {error}");
                        continue;
                    }
                    first_error = Some(error.to_string());
                    break;
                }
            }
        }
    } else {
        let run_targets = runnable
            .into_iter()
            .enumerate()
            .map(|(position, index)| ParallelRunTarget {
                position,
                key: summary_key(&graph.projects[index]),
                display_prefix: graph.projects[index].name.clone(),
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
            ParallelRunOptions {
                command: &command,
                args: &args,
                if_present,
                config,
                no_bail,
                concurrency,
                capture_output: capture_parallel_output,
            },
        );
        for outcome in outcomes {
            if capture_parallel_output {
                print_captured_output(
                    &outcome.display_prefix,
                    &command,
                    &outcome.stdout,
                    &outcome.stderr,
                    show_prefix,
                    use_stderr,
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
            if let Some(error_message) = outcome.error_message {
                if no_bail {
                    eprintln!("Script execution failed but --no-bail was set: {error_message}");
                    continue;
                }
                if first_error.is_none() {
                    first_error = Some(error_message);
                }
            }
        }
    }

    if let Some(summary) = summary.as_ref() {
        write_execution_summary(&target_dir, summary)?;
    }
    if let Some(error_message) = first_error {
        miette::bail!("{error_message}");
    }

    Ok(())
}

fn parse_workspace_concurrency(value: &str) -> Option<usize> {
    if value.eq_ignore_ascii_case("infinity") {
        return Some(usize::MAX);
    }
    value.parse::<usize>().ok().filter(|value| *value > 0)
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

#[derive(Debug, Clone)]
struct ParallelRunTarget {
    position: usize,
    key: String,
    display_prefix: String,
    manifest_path: PathBuf,
}

#[derive(Debug, Clone)]
struct ParallelRunOutcome {
    position: usize,
    key: String,
    display_prefix: String,
    duration_ms: f64,
    stdout: String,
    stderr: String,
    error_message: Option<String>,
}

#[derive(Clone, Copy)]
struct ParallelRunOptions<'a> {
    command: &'a str,
    args: &'a [String],
    if_present: bool,
    config: &'a Npmrc,
    no_bail: bool,
    concurrency: usize,
    capture_output: bool,
}

fn run_targets_parallel(
    run_targets: Vec<ParallelRunTarget>,
    opts: ParallelRunOptions<'_>,
) -> Vec<ParallelRunOutcome> {
    let queue =
        Arc::new(Mutex::new(run_targets.into_iter().collect::<VecDeque<ParallelRunTarget>>()));
    let outcomes = Arc::new(Mutex::new(Vec::<ParallelRunOutcome>::new()));
    let should_stop = Arc::new(AtomicBool::new(false));
    let worker_count = opts.concurrency.max(1);
    let mut workers = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let queue = Arc::clone(&queue);
        let outcomes = Arc::clone(&outcomes);
        let should_stop = Arc::clone(&should_stop);
        let command = opts.command.to_string();
        let args = opts.args.to_vec();
        let config = opts.config.clone();
        let if_present = opts.if_present;
        let no_bail = opts.no_bail;
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
                let ParallelRunTarget { position, key, display_prefix, manifest_path } = target;

                let started = Instant::now();
                let output_mode = if capture_output {
                    ScriptOutputMode::Capture
                } else {
                    ScriptOutputMode::Inherit
                };
                let result = run_named_script_with_output_mode(
                    manifest_path,
                    &command,
                    &args,
                    if_present,
                    false,
                    &config,
                    output_mode,
                );
                let duration_ms = started.elapsed().as_secs_f64() * 1_000.0;

                let (stdout, stderr, error_message) = match result {
                    Ok(output) => (output.stdout, output.stderr, None),
                    Err(error) => (String::new(), String::new(), Some(error.to_string())),
                };
                if error_message.is_some() && !no_bail {
                    should_stop.store(true, Ordering::Relaxed);
                }

                let mut outcomes = outcomes.lock().expect("lock parallel outcomes");
                outcomes.push(ParallelRunOutcome {
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

fn print_captured_output(
    display_prefix: &str,
    command: &str,
    stdout: &str,
    stderr: &str,
    show_prefix: bool,
    use_stderr: bool,
) {
    if show_prefix {
        print_prefixed_lines(stdout, display_prefix, command, false, use_stderr);
        print_prefixed_lines(stderr, display_prefix, command, true, use_stderr);
    } else if use_stderr {
        eprint!("{stdout}");
        eprint!("{stderr}");
    } else {
        print!("{stdout}");
        eprint!("{stderr}");
    }
}

fn print_prefixed_lines(
    text: &str,
    display_prefix: &str,
    command: &str,
    is_stderr: bool,
    use_stderr: bool,
) {
    for chunk in text.split_inclusive('\n') {
        let (line, has_newline) =
            chunk.strip_suffix('\n').map_or((chunk, false), |line| (line, true));
        if use_stderr || is_stderr {
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

#[derive(Debug)]
struct WorkspaceProject {
    manifest_path: PathBuf,
    package_dir: PathBuf,
    relative_dir: PathBuf,
    name: String,
    dependency_names_all: Vec<String>,
    dependency_names_prod: Vec<String>,
}

#[derive(Debug)]
struct WorkspaceGraph {
    workspace_root: PathBuf,
    projects: Vec<WorkspaceProject>,
    dependencies_all: Vec<Vec<usize>>,
    dependencies_prod: Vec<Vec<usize>>,
    dependents_all: Vec<Vec<usize>>,
    dependents_prod: Vec<Vec<usize>>,
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
                let dependency_names_all = collect_dependency_names(&manifest, false);
                let dependency_names_prod = collect_dependency_names(&manifest, true);
                Some(WorkspaceProject {
                    manifest_path,
                    package_dir,
                    relative_dir,
                    name,
                    dependency_names_all,
                    dependency_names_prod,
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
        let dependencies_prod = projects
            .iter()
            .map(|project| {
                project
                    .dependency_names_prod
                    .iter()
                    .filter_map(|name| by_name.get(name).copied())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        let dependents_all = reverse_edges(&dependencies_all, projects.len());
        let dependents_prod = reverse_edges(&dependencies_prod, projects.len());

        Self {
            workspace_root: workspace_root.to_path_buf(),
            projects,
            dependencies_all,
            dependencies_prod,
            dependents_all,
            dependents_prod,
        }
    }
}

fn reverse_edges(edges: &[Vec<usize>], size: usize) -> Vec<Vec<usize>> {
    let mut reverse = vec![Vec::new(); size];
    for (source, deps) in edges.iter().enumerate() {
        for &target in deps {
            reverse[target].push(source);
        }
    }
    reverse
}

fn collect_dependency_names(manifest: &PackageManifest, prod_only: bool) -> Vec<String> {
    let mut names = BTreeSet::new();
    for field in ["dependencies", "optionalDependencies", "peerDependencies"] {
        if let Some(object) = manifest.value().get(field).and_then(|value| value.as_object()) {
            names.extend(object.keys().cloned());
        }
    }
    if !prod_only
        && let Some(object) =
            manifest.value().get("devDependencies").and_then(|value| value.as_object())
    {
        names.extend(object.keys().cloned());
    }
    names.into_iter().collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Traversal {
    None,
    DependenciesWithSelf,
    DependenciesOnly,
    DependentsWithSelf,
    DependentsOnly,
}

#[derive(Debug, Clone)]
enum SelectorCore {
    All,
    NamePattern(String),
    Cwd,
    RelativeDir(PathBuf),
    WorkspaceDir(PathBuf),
    Since,
    WorkspaceDirSince { dir: PathBuf },
}

#[derive(Debug, Clone)]
struct ParsedSelector {
    traversal: Traversal,
    core: SelectorCore,
    since_ref: Option<String>,
}

fn parse_selector(selector: &str) -> ParsedSelector {
    let (traversal, rest) = if let Some(value) = selector.strip_prefix("...^") {
        (Traversal::DependentsOnly, value)
    } else if let Some(value) = selector.strip_prefix("...") {
        (Traversal::DependentsWithSelf, value)
    } else if let Some(value) = selector.strip_suffix("^...") {
        (Traversal::DependenciesOnly, value)
    } else if let Some(value) = selector.strip_suffix("...") {
        (Traversal::DependenciesWithSelf, value)
    } else {
        (Traversal::None, selector)
    };

    let rest = rest.trim();
    if rest.is_empty() {
        return ParsedSelector { traversal, core: SelectorCore::All, since_ref: None };
    }
    if rest == "." {
        return ParsedSelector { traversal, core: SelectorCore::Cwd, since_ref: None };
    }
    if let Some(value) = rest.strip_prefix("./") {
        return ParsedSelector {
            traversal,
            core: SelectorCore::RelativeDir(PathBuf::from(value)),
            since_ref: None,
        };
    }
    if rest.starts_with('[') && rest.ends_with(']') {
        let since = rest[1..rest.len() - 1].to_string();
        return ParsedSelector { traversal, core: SelectorCore::Since, since_ref: Some(since) };
    }
    if rest.starts_with('{')
        && let Some(close) = rest.find('}')
    {
        let dir = PathBuf::from(&rest[1..close]);
        let suffix = &rest[close + 1..];
        if suffix.is_empty() {
            return ParsedSelector {
                traversal,
                core: SelectorCore::WorkspaceDir(dir),
                since_ref: None,
            };
        }
        if suffix.starts_with('[') && suffix.ends_with(']') {
            let since = suffix[1..suffix.len() - 1].to_string();
            return ParsedSelector {
                traversal,
                core: SelectorCore::WorkspaceDirSince { dir },
                since_ref: Some(since),
            };
        }
    }

    ParsedSelector { traversal, core: SelectorCore::NamePattern(rest.to_string()), since_ref: None }
}

#[derive(Debug, Default, Clone)]
struct ChangedPackages {
    all: HashSet<usize>,
    non_test: HashSet<usize>,
}

fn resolve_filter_selection(
    graph: &WorkspaceGraph,
    target_dir: &Path,
    filters: &[FilterSelector],
    changed_files_ignore_patterns: &[String],
    test_patterns: &[String],
) -> Vec<usize> {
    let mut include = HashSet::<usize>::new();
    let mut exclude = HashSet::<usize>::new();
    let mut changed_cache = HashMap::<String, ChangedPackages>::new();

    let mut has_positive = false;
    for selector in filters {
        let is_negative = selector.selector.starts_with('!');
        let selector_text =
            selector.selector.strip_prefix('!').unwrap_or(&selector.selector).to_string();
        let parsed = parse_selector(&selector_text);
        let changed = parsed.since_ref.as_ref().map(|since| {
            changed_cache
                .entry(since.clone())
                .or_insert_with(|| {
                    compute_changed_packages(
                        graph,
                        since,
                        changed_files_ignore_patterns,
                        test_patterns,
                    )
                })
                .to_owned()
        });

        let selected =
            select_from_graph(graph, target_dir, &parsed, changed.as_ref(), selector.prod_only);

        if is_negative {
            exclude.extend(selected);
        } else {
            has_positive = true;
            include.extend(selected);
        }
    }

    if !has_positive {
        include.extend(0..graph.projects.len());
    }

    include.retain(|index| !exclude.contains(index));
    let mut result = include.into_iter().collect::<Vec<_>>();
    result.sort_by_key(|index| graph.projects[*index].manifest_path.clone());
    result
}

fn select_from_graph(
    graph: &WorkspaceGraph,
    target_dir: &Path,
    selector: &ParsedSelector,
    changed: Option<&ChangedPackages>,
    prod_only: bool,
) -> HashSet<usize> {
    let base = base_selection(graph, target_dir, &selector.core, changed, false);

    let traversal_seed = match selector.core {
        SelectorCore::Since | SelectorCore::WorkspaceDirSince { .. }
            if selector.traversal == Traversal::DependentsWithSelf
                || selector.traversal == Traversal::DependentsOnly =>
        {
            base_selection(graph, target_dir, &selector.core, changed, true)
        }
        _ => base.clone(),
    };

    match selector.traversal {
        Traversal::None => base,
        Traversal::DependenciesWithSelf => traverse(graph, &base, true, prod_only, true),
        Traversal::DependenciesOnly => traverse(graph, &base, false, prod_only, true),
        Traversal::DependentsWithSelf => {
            let mut selected = traverse(graph, &traversal_seed, true, prod_only, false);
            if matches!(selector.core, SelectorCore::Since | SelectorCore::WorkspaceDirSince { .. })
            {
                selected.extend(base);
            }
            selected
        }
        Traversal::DependentsOnly => traverse(graph, &traversal_seed, false, prod_only, false),
    }
}

fn base_selection(
    graph: &WorkspaceGraph,
    target_dir: &Path,
    core: &SelectorCore,
    changed: Option<&ChangedPackages>,
    non_test_only: bool,
) -> HashSet<usize> {
    match core {
        SelectorCore::All => (0..graph.projects.len()).collect(),
        SelectorCore::NamePattern(pattern) => graph
            .projects
            .iter()
            .enumerate()
            .filter(|(_, project)| matches_filter_selector(&project.name, pattern))
            .map(|(index, _)| index)
            .collect(),
        SelectorCore::Cwd => graph
            .projects
            .iter()
            .enumerate()
            .filter(|(_, project)| project.package_dir.starts_with(target_dir))
            .map(|(index, _)| index)
            .collect(),
        SelectorCore::RelativeDir(dir) => {
            let absolute = target_dir.join(dir);
            graph
                .projects
                .iter()
                .enumerate()
                .filter(|(_, project)| project.package_dir.starts_with(&absolute))
                .map(|(index, _)| index)
                .collect()
        }
        SelectorCore::WorkspaceDir(dir) => graph
            .projects
            .iter()
            .enumerate()
            .filter(|(_, project)| project.relative_dir.starts_with(dir))
            .map(|(index, _)| index)
            .collect(),
        SelectorCore::Since => changed
            .map(
                |changed| {
                    if non_test_only { changed.non_test.clone() } else { changed.all.clone() }
                },
            )
            .unwrap_or_default(),
        SelectorCore::WorkspaceDirSince { dir } => {
            let changed = changed
                .map(
                    |changed| {
                        if non_test_only { changed.non_test.clone() } else { changed.all.clone() }
                    },
                )
                .unwrap_or_default();
            changed
                .into_iter()
                .filter(|index| graph.projects[*index].relative_dir.starts_with(dir))
                .collect()
        }
    }
}

fn traverse(
    graph: &WorkspaceGraph,
    seed: &HashSet<usize>,
    include_seed: bool,
    prod_only: bool,
    forward: bool,
) -> HashSet<usize> {
    let edges = match (forward, prod_only) {
        (true, true) => &graph.dependencies_prod,
        (true, false) => &graph.dependencies_all,
        (false, true) => &graph.dependents_prod,
        (false, false) => &graph.dependents_all,
    };

    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    for &index in seed {
        queue.push_back(index);
    }

    while let Some(index) = queue.pop_front() {
        if !visited.insert(index) {
            continue;
        }
        for &next in &edges[index] {
            queue.push_back(next);
        }
    }

    if !include_seed {
        visited.retain(|index| !seed.contains(index));
    }
    visited
}

fn compute_changed_packages(
    graph: &WorkspaceGraph,
    since_ref: &str,
    changed_files_ignore_patterns: &[String],
    test_patterns: &[String],
) -> ChangedPackages {
    let mut changed = ChangedPackages::default();
    let output = Command::new("git")
        .args(["diff", "--name-only", since_ref, "--"])
        .current_dir(&graph.workspace_root)
        .output();
    let Ok(output) = output else {
        return changed;
    };
    if !output.status.success() {
        return changed;
    }

    let ignore_patterns = changed_files_ignore_patterns
        .iter()
        .filter_map(|pattern| Pattern::new(pattern).ok())
        .collect::<Vec<_>>();
    let test_patterns =
        test_patterns.iter().filter_map(|pattern| Pattern::new(pattern).ok()).collect::<Vec<_>>();

    let mut per_package_non_test = HashMap::<usize, bool>::new();
    let mut by_depth = graph
        .projects
        .iter()
        .enumerate()
        .map(|(index, project)| (index, project.relative_dir.components().count()))
        .collect::<Vec<_>>();
    by_depth.sort_by_key(|(_, depth)| std::cmp::Reverse(*depth));

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let normalized = line.replace('\\', "/");
        if normalized.is_empty()
            || ignore_patterns.iter().any(|pattern| pattern.matches(&normalized))
        {
            continue;
        }
        let file_path = PathBuf::from(&normalized);
        let matched = by_depth
            .iter()
            .find(|(index, _)| file_path.starts_with(&graph.projects[*index].relative_dir))
            .map(|(index, _)| *index);
        let Some(index) = matched else {
            continue;
        };
        changed.all.insert(index);

        let is_test = !test_patterns.is_empty()
            && test_patterns.iter().any(|pattern| pattern.matches(&normalized));
        let entry = per_package_non_test.entry(index).or_insert(false);
        if !is_test {
            *entry = true;
        }
    }

    if test_patterns.is_empty() {
        changed.non_test = changed.all.clone();
    } else {
        changed.non_test = per_package_non_test
            .into_iter()
            .filter_map(|(index, non_test)| non_test.then_some(index))
            .collect();
    }

    changed
}

fn matches_filter_selector(name: &str, selector: &str) -> bool {
    Pattern::new(selector).map(|pattern| pattern.matches(name)).unwrap_or(name == selector)
}

fn find_workspace_root(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        if dir.join("pnpm-workspace.yaml").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn collect_package_json_paths(workspace_root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, result: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let file_name = entry.file_name().to_string_lossy().to_string();

            if file_type.is_file() && file_name == "package.json" {
                result.push(path);
                continue;
            }
            if !file_type.is_dir() {
                continue;
            }
            if matches!(file_name.as_str(), "node_modules" | ".git" | "target") {
                continue;
            }
            walk(&path, result);
        }
    }

    let mut result = Vec::new();
    walk(workspace_root, &mut result);
    result
}
