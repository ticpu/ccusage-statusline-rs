use std::sync::OnceLock;
use std::time::Instant;

/// Phase timings go to stderr when `CCUSAGE_TIMING` is set. The statusline has a hard
/// latency budget but its work is spread across cache, filesystem and network phases,
/// and a wall-clock total says nothing about which one regressed.
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("CCUSAGE_TIMING").is_some())
}

/// Run `f`, reporting how long it took under `name`.
pub fn phase<T>(name: &str, f: impl FnOnce() -> T) -> T {
    if !enabled() {
        return f();
    }
    let start = Instant::now();
    let out = f();
    report(
        name,
        start
            .elapsed()
            .as_secs_f64()
            * 1000.0,
        None,
    );
    out
}

/// Like `phase`, but also reports a count the phase produced (files scanned, entries
/// parsed) so a slow phase can be told apart from a phase given too much work.
pub fn phase_counted<T>(name: &str, f: impl FnOnce() -> (T, usize)) -> T {
    if !enabled() {
        return f().0;
    }
    let start = Instant::now();
    let (out, count) = f();
    report(
        name,
        start
            .elapsed()
            .as_secs_f64()
            * 1000.0,
        Some(count),
    );
    out
}

fn report(name: &str, millis: f64, count: Option<usize>) {
    // Not tty-gated: setting the variable is the request, and redirecting stderr to a
    // file is the normal way to capture a profile from a real Claude Code invocation.
    match count {
        Some(n) => eprintln!("timing {name}: {millis:.1}ms ({n})"),
        None => eprintln!("timing {name}: {millis:.1}ms"),
    }
}
