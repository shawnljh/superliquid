#[macro_export]
macro_rules! replica_log {
    ($node_id:expr, $($arg:tt)*) => {{
        use chrono::Local;
        let now = Local::now().format("%H:%M:%S%.3f");
        let prefix = format!(
            "\x1b[90m[{}]\x1b[0m \x1b[94m[Replica {}]\x1b[0m",
            now, $node_id
        );
        println!("{} {}", prefix, format!($($arg)*));
    }};
}

#[macro_export]
macro_rules! replica_debug {
    ($node_id:expr, $curr_view:expr, $($arg:tt)*) => {{
        if std::env::var("REPLICA_DEBUG").map(|v| v == "true").unwrap_or(false) {
        use chrono::Local;
        let now = Local::now().format("%H:%M:%S%.3f");
        let prefix = format!(
            "\x1b[90m[{}]\x1b[0m \x1b[91m[Replica {} @ view {}]\x1b[0m",
            now, $node_id, $curr_view
        );
        println!("{} {}", prefix, format!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! pacemaker_log {
    ($($arg:tt)*) => {{
        use chrono::Local;
        let now = Local::now().format("%H:%M:%S%.3f");
        let prefix = format!(
            "\x1b[90m[{}]\x1b[0m \x1b[96m[Pacemaker]\x1b[0m",
            now,
        );
        println!("{} {}", prefix, format!($($arg)*));
    }};
}
