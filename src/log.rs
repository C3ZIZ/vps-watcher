//! Minimal leveled logging to stderr; journald adds timestamps when run as a service.

/// Logs an informational line.
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { eprintln!("INFO  {}", format_args!($($arg)*)) };
}

/// Logs a recoverable problem.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { eprintln!("WARN  {}", format_args!($($arg)*)) };
}
