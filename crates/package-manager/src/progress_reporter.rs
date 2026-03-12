use std::collections::HashSet;
use std::io::{IsTerminal, Write, stderr};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallReporter {
    Default,
    AppendOnly,
    Silent,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProgressStats {
    pub resolved: usize,
    pub reused: usize,
    pub downloaded: usize,
    pub added: usize,
}

#[derive(Debug)]
struct State {
    last_draw: Instant,
    stats: ProgressStats,
    rendered: bool,
    reporter: InstallReporter,
    prefix: Option<String>,
    deprecations: HashSet<String>,
    #[allow(dead_code)]
    shown_warnings: usize,
    suppressed_warnings: usize,
}

impl State {
    fn new(reporter: InstallReporter, prefix: Option<String>) -> Self {
        let now = Instant::now();
        Self {
            last_draw: now.checked_sub(Duration::from_millis(200)).unwrap_or(now),
            stats: ProgressStats::default(),
            rendered: false,
            reporter,
            prefix,
            deprecations: HashSet::new(),
            shown_warnings: 0,
            suppressed_warnings: 0,
        }
    }
}

static STATE: OnceLock<Mutex<Option<State>>> = OnceLock::new();
static LAST_FINISHED: OnceLock<Mutex<Option<ProgressStats>>> = OnceLock::new();

fn state_mutex() -> &'static Mutex<Option<State>> {
    STATE.get_or_init(|| Mutex::new(None))
}

fn last_finished_mutex() -> &'static Mutex<Option<ProgressStats>> {
    LAST_FINISHED.get_or_init(|| Mutex::new(None))
}

fn format_progress(stats: ProgressStats, done: bool) -> String {
    let mut line = format!(
        "Progress: resolved {}, reused {}, downloaded {}, added {}",
        stats.resolved, stats.reused, stats.downloaded, stats.added
    );
    if done {
        line.push_str(", done");
    }
    line
}

fn format_warn(message: &str) -> String {
    format!("WARN {message}")
}

#[allow(dead_code)]
fn format_info(message: &str) -> String {
    format!("info: {message}")
}

fn format_deprecation_message(
    is_prefixed: bool,
    package_name: &str,
    version: &str,
    message: &str,
) -> String {
    if is_prefixed {
        return format_warn(&format!("deprecated {package_name}@{version}"));
    }
    format_warn(&format!("deprecated {package_name}@{version}: {message}"))
}

pub fn format_prefix_label(prefix: &str) -> String {
    const PREFIX_WIDTH: usize = 41;
    if prefix.len() <= PREFIX_WIDTH {
        return prefix.to_string();
    }
    if let Some((_, last)) = prefix.rsplit_once('/')
        && last.len() + 4 <= PREFIX_WIDTH
    {
        return format!(".../{last}");
    }
    format!("...{}", &prefix[prefix.len() - (PREFIX_WIDTH - 3)..])
}

fn format_with_prefix(prefix: Option<&str>, line: &str) -> String {
    match prefix {
        Some(prefix) => format!("{:<41} | {line}", format_prefix_label(prefix)),
        None => line.to_string(),
    }
}

fn draw(state: &mut State, done: bool, force: bool) {
    if !force && state.last_draw.elapsed() < Duration::from_millis(70) {
        return;
    }
    state.last_draw = Instant::now();
    let mut err = stderr();
    let line = format_with_prefix(state.prefix.as_deref(), &format_progress(state.stats, done));

    match state.reporter {
        InstallReporter::Default => {
            if err.is_terminal() {
                let _ = write!(err, "\r\x1b[2K{line}");
                if done {
                    let _ = writeln!(err);
                }
                state.rendered = !done;
            } else {
                state.rendered = false;
            }
        }
        InstallReporter::AppendOnly => {
            let _ = writeln!(err, "{line}");
            state.rendered = false;
        }
        InstallReporter::Silent => {
            state.rendered = false;
        }
    }
    let _ = err.flush();
}

fn print_message(state: Option<&mut State>, message: &str) {
    let mut err = stderr();
    match state {
        Some(state) => match state.reporter {
            InstallReporter::Default => {
                if err.is_terminal() && state.rendered {
                    let _ = write!(err, "\r\x1b[2K");
                }
                let _ = writeln!(err, "{}", format_with_prefix(state.prefix.as_deref(), message));
                state.rendered = false;
            }
            InstallReporter::AppendOnly => {
                let _ = writeln!(err, "{}", format_with_prefix(state.prefix.as_deref(), message));
            }
            InstallReporter::Silent => {}
        },
        None => {
            let _ = writeln!(err, "{message}");
        }
    }
    let _ = err.flush();
}

fn update(update_counter: impl FnOnce(&mut ProgressStats)) {
    let mutex = state_mutex();
    let mut guard = mutex.lock().expect("progress mutex");
    let Some(state) = guard.as_mut() else {
        return;
    };
    update_counter(&mut state.stats);
    draw(state, false, false);
}

pub fn start(
    _direct_dependencies: usize,
    _frozen_lockfile: bool,
    reporter: InstallReporter,
    prefix: Option<&str>,
) {
    *last_finished_mutex().lock().expect("last finished progress mutex") = None;
    let mutex = state_mutex();
    let mut guard = mutex.lock().expect("progress mutex");
    *guard = Some(State::new(reporter, prefix.map(ToOwned::to_owned)));
}

pub fn resolved() {
    update(|stats| stats.resolved += 1);
}

pub fn reused() {
    update(|stats| stats.reused += 1);
}

pub fn downloaded() {
    update(|stats| stats.downloaded += 1);
}

pub fn added() {
    update(|stats| stats.added += 1);
}

pub fn finish(success: bool) -> Option<ProgressStats> {
    let mutex = state_mutex();
    let mut guard = mutex.lock().expect("progress mutex");
    let state = guard.as_mut()?;

    if state.reporter != InstallReporter::AppendOnly && state.suppressed_warnings > 0 {
        let summary = format_warn(&format!("{} other warnings", state.suppressed_warnings));
        print_message(Some(state), &summary);
        state.suppressed_warnings = 0;
    }

    if success && state.rendered || state.reporter == InstallReporter::AppendOnly {
        draw(state, true, true);
    } else if !success && state.reporter == InstallReporter::Default && stderr().is_terminal() {
        let mut err = stderr();
        if state.rendered {
            let _ = writeln!(err);
        }
        let _ = err.flush();
    }

    let stats = state.stats;
    *last_finished_mutex().lock().expect("last finished progress mutex") = Some(stats);
    *guard = None;
    Some(stats)
}

pub fn last_finished() -> Option<ProgressStats> {
    *last_finished_mutex().lock().expect("last finished progress mutex")
}

pub fn log(message: &str) {
    let mutex = state_mutex();
    let mut guard = mutex.lock().expect("progress mutex");
    print_message(guard.as_mut(), message);
}

#[allow(dead_code)]
pub fn info(message: &str) {
    let mutex = state_mutex();
    let mut guard = mutex.lock().expect("progress mutex");
    match guard.as_mut() {
        Some(state) => match state.reporter {
            InstallReporter::Silent => {}
            _ => print_message(Some(state), &format_info(message)),
        },
        None => print_message(None, &format_info(message)),
    }
}

#[allow(dead_code)]
pub fn warn(message: &str) {
    const MAX_SHOWN_WARNINGS: usize = 5;

    let mutex = state_mutex();
    let mut guard = mutex.lock().expect("progress mutex");
    let Some(state) = guard.as_mut() else {
        print_message(None, &format_warn(message));
        return;
    };

    if state.reporter == InstallReporter::AppendOnly || state.shown_warnings < MAX_SHOWN_WARNINGS {
        state.shown_warnings += 1;
        print_message(Some(state), &format_warn(message));
        return;
    }

    state.suppressed_warnings += 1;
}

pub fn deprecation(package_name: &str, version: &str, message: &str) {
    let key = format!("{package_name}@{version}");
    let mutex = state_mutex();
    let mut guard = mutex.lock().expect("progress mutex");
    let Some(state) = guard.as_mut() else {
        print_message(None, &format_deprecation_message(false, package_name, version, message));
        return;
    };
    if !state.deprecations.insert(key) {
        return;
    }
    let formatted =
        format_deprecation_message(state.prefix.is_some(), package_name, version, message);
    print_message(Some(state), &formatted);
}

#[cfg(test)]
mod tests {
    use super::{
        InstallReporter, deprecation, finish, format_deprecation_message, format_info,
        format_prefix_label, info, start, state_mutex, warn,
    };

    #[test]
    fn format_prefix_label_truncates_long_prefixes_from_the_left() {
        let formatted = format_prefix_label("loooooooooooooooooooooooooooooooooong-pkg-4");
        assert_eq!(formatted.len(), 41);
        assert!(formatted.starts_with("..."));
        assert!(formatted.ends_with("ong-pkg-4"));
    }

    #[test]
    fn format_prefix_label_prefers_last_path_segment_for_long_paths() {
        let formatted = format_prefix_label("loooooooooooooooooooooooooooooooooong/pkg-3");
        assert_eq!(formatted, ".../pkg-3");
    }

    #[test]
    fn deprecation_messages_are_deduped_per_run() {
        start(0, false, InstallReporter::AppendOnly, None);
        deprecation("foo", "1.0.0", "old");
        deprecation("foo", "1.0.0", "old");

        let guard = state_mutex().lock().expect("progress mutex");
        let state = guard.as_ref().expect("state");
        assert_eq!(state.deprecations.len(), 1);
        drop(guard);

        let _ = finish(true);
    }

    #[test]
    fn deprecation_message_includes_reason_for_non_recursive_root_output() {
        assert_eq!(
            format_deprecation_message(false, "foo", "1.0.0", "old"),
            "WARN deprecated foo@1.0.0: old"
        );
    }

    #[test]
    fn deprecation_message_omits_reason_for_prefixed_recursive_output() {
        assert_eq!(
            format_deprecation_message(true, "foo", "1.0.0", "old"),
            "WARN deprecated foo@1.0.0"
        );
    }

    #[test]
    fn info_message_uses_pnpm_style_prefix() {
        assert_eq!(
            format_info("pkg@1.0.0 is an optional dependency and failed compatibility check."),
            "info: pkg@1.0.0 is an optional dependency and failed compatibility check."
        );
    }

    #[test]
    fn warnings_are_collapsed_after_five_when_not_append_only() {
        start(0, false, InstallReporter::Default, None);
        for idx in 0..7 {
            warn(&format!("issue {idx}"));
        }

        let guard = state_mutex().lock().expect("progress mutex");
        let state = guard.as_ref().expect("state");
        assert_eq!(state.shown_warnings, 5);
        assert_eq!(state.suppressed_warnings, 2);
        drop(guard);

        let _ = finish(true);
    }

    #[test]
    fn info_does_not_increment_warning_counters() {
        start(0, false, InstallReporter::Default, None);
        info("something happened");

        let guard = state_mutex().lock().expect("progress mutex");
        let state = guard.as_ref().expect("state");
        assert_eq!(state.shown_warnings, 0);
        assert_eq!(state.suppressed_warnings, 0);
        drop(guard);

        let _ = finish(true);
    }

    #[test]
    fn append_only_warnings_are_not_collapsed() {
        start(0, false, InstallReporter::AppendOnly, None);
        for idx in 0..7 {
            warn(&format!("issue {idx}"));
        }

        let guard = state_mutex().lock().expect("progress mutex");
        let state = guard.as_ref().expect("state");
        assert_eq!(state.shown_warnings, 7);
        assert_eq!(state.suppressed_warnings, 0);
        drop(guard);

        let _ = finish(true);
    }
}
