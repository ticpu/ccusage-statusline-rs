/// Stderr diagnostic gated on stderr being a terminal: Claude Code neither shows nor
/// discards statusline stderr predictably, so only the interactive run can surface it.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            eprintln!($($arg)*);
        }
    };
}
