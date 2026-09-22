use chrono::Local;

pub trait Logger: Send + Sync {
    fn log(&self, level: &str, msg: &str);
}

pub struct ConsoleLogger {
    node_id: usize,
}

impl ConsoleLogger {
    pub fn new(node_id: usize) -> Self {
        Self { node_id }
    }
}

impl Logger for ConsoleLogger {
    fn log(&self, level: &str, msg: &str) {
        let now = Local::now().format("%H:%M:%S%.3f");

        let formatted = format!(
            "\x1b[90m[{}]\x1b[0m \x1b[34m[Node {}]\x1b[0m \x1b[93m[{}]\x1b[0m {}",
            now,
            self.node_id,
            level.to_uppercase(),
            msg
        );

        match level {
            "warn" | "error" => eprintln!("{}", formatted),
            _ => println!("{}", formatted),
        }
    }
}

#[cfg(test)]
use std::sync::{Arc, Mutex};
#[cfg(test)]
pub struct StubLogger {
    pub logs: Arc<Mutex<Vec<(String, String)>>>,
}

#[cfg(test)]
impl StubLogger {
    pub fn new() -> Self {
        Self {
            logs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn entries(&self) -> Arc<Mutex<Vec<(String, String)>>> {
        Arc::clone(&self.logs)
    }
}

#[cfg(test)]
impl Default for StubLogger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl Logger for StubLogger {
    fn log(&self, level: &str, msg: &str) {
        self.logs
            .lock()
            .unwrap()
            .push((level.to_string(), msg.to_string()));
    }
}
