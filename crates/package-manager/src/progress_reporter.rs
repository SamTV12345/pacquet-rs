use std::io::{IsTerminal, Write, stderr};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct State {
    start: Instant,
    last_draw: Instant,
    direct_dependencies: usize,
    frozen_lockfile: bool,
    resolved: usize,
    fetched: usize,
    linked: usize,
    phase: &'static str,
    spinner_index: usize,
    rendered_lines: usize,
}

impl State {
    fn new(direct_dependencies: usize, frozen_lockfile: bool) -> Self {
        let now = Instant::now();
        Self {
            start: now,
            last_draw: now.checked_sub(Duration::from_millis(200)).unwrap_or(now),
            direct_dependencies,
            frozen_lockfile,
            resolved: 0,
            fetched: 0,
            linked: 0,
            phase: "starting",
            spinner_index: 0,
            rendered_lines: 0,
        }
    }

    fn mode(&self) -> &'static str {
        if self.frozen_lockfile { "frozen" } else { "regular" }
    }
}

static STATE: OnceLock<Mutex<Option<State>>> = OnceLock::new();

fn state_mutex() -> &'static Mutex<Option<State>> {
    STATE.get_or_init(|| Mutex::new(None))
}

fn spinner_frame(state: &mut State) -> &'static str {
    const FRAMES: [&str; 4] = ["-", "\\", "|", "/"];
    state.spinner_index = (state.spinner_index + 1) % FRAMES.len();
    FRAMES[state.spinner_index]
}

fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let minutes = secs / 60;
    let seconds = secs % 60;
    format!("{minutes:02}:{seconds:02}")
}

fn format_rate(value: usize, elapsed_secs: f64) -> String {
    format!("{:>7.1}/s", value as f64 / elapsed_secs)
}

fn direct_progress_bar(done: usize, total: usize, width: usize) -> String {
    if total == 0 {
        return format!("[{}]", "=".repeat(width));
    }
    let filled = ((done.min(total) as f64 / total as f64) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("[{}{}]", "=".repeat(filled), "-".repeat(width - filled))
}

fn render_lines(state: &mut State, phase_label: &str) -> Vec<String> {
    let elapsed = state.start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64().max(0.001);
    let direct_done = state.fetched.min(state.direct_dependencies);
    let direct_percent = if state.direct_dependencies == 0 {
        100.0
    } else {
        100.0 * direct_done as f64 / state.direct_dependencies as f64
    };
    let bar = direct_progress_bar(direct_done, state.direct_dependencies, 26);
    let spinner = spinner_frame(state);

    vec![
        format!("pacquet {spinner} {} [{}]", phase_label, state.mode()),
        format!(
            "  elapsed {}    direct {:>4}    lockfile {}",
            format_elapsed(elapsed),
            state.direct_dependencies,
            if state.frozen_lockfile { "frozen" } else { "mutable" }
        ),
        format!(
            "  resolved {:>6}    fetched {:>6}    linked {:>6}",
            state.resolved, state.fetched, state.linked
        ),
        format!(
            "  rates    resolve {}    fetch {}    link {}",
            format_rate(state.resolved, elapsed_secs),
            format_rate(state.fetched, elapsed_secs),
            format_rate(state.linked, elapsed_secs)
        ),
        format!("  direct install {bar} {:>5.1}%", direct_percent),
    ]
}

fn draw(state: &mut State, phase_label: &str, force: bool) {
    if !force && state.last_draw.elapsed() < Duration::from_millis(70) {
        return;
    }
    state.last_draw = Instant::now();

    let lines = render_lines(state, phase_label);
    let mut err = stderr();

    if state.rendered_lines > 0 {
        let _ = write!(err, "\x1b[{}A", state.rendered_lines);
    }

    for line in &lines {
        let _ = write!(err, "\r\x1b[2K{line}\n");
    }
    let _ = err.flush();
    state.rendered_lines = lines.len();
}

fn update(phase: &'static str, update_counter: impl FnOnce(&mut State)) {
    let mutex = state_mutex();
    let mut guard = mutex.lock().expect("progress mutex");
    let Some(state) = guard.as_mut() else {
        return;
    };
    update_counter(state);
    state.phase = phase;
    draw(state, phase, false);
}

pub fn start(direct_dependencies: usize, frozen_lockfile: bool) {
    if !stderr().is_terminal() {
        return;
    }
    let mutex = state_mutex();
    let mut guard = mutex.lock().expect("progress mutex");
    *guard = Some(State::new(direct_dependencies, frozen_lockfile));
    if let Some(state) = guard.as_mut() {
        draw(state, "starting", true);
    }
}

pub fn resolved() {
    update("resolving", |state| state.resolved += 1);
}

pub fn fetched() {
    update("fetching", |state| state.fetched += 1);
}

pub fn linked() {
    update("linking", |state| state.linked += 1);
}

pub fn finish(success: bool) {
    let mutex = state_mutex();
    let mut guard = mutex.lock().expect("progress mutex");
    let Some(state) = guard.as_mut() else {
        return;
    };

    let phase = if success { "done" } else { "failed" };
    draw(state, phase, true);

    let mut err = stderr();
    let _ = writeln!(err, "\r\x1b[2K");
    let elapsed = state.start.elapsed().as_secs_f32();
    let _ = writeln!(
        err,
        "pacquet [{}] {phase} | direct: {} | resolved: {} | fetched: {} | linked: {} | {:.1}s",
        state.mode(),
        state.direct_dependencies,
        state.resolved,
        state.fetched,
        state.linked,
        elapsed
    );
    let _ = err.flush();
    *guard = None;
}
