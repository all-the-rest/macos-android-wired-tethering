//! Small command runner abstraction for macOS system tools.

use std::process::Command;

pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|error| format!("cannot execute {program}: {error}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr.trim();
            return Err(format!(
                "{program} {} failed with {}: {}",
                args.join(" "),
                output.status,
                if stderr.is_empty() {
                    "no diagnostic"
                } else {
                    stderr
                }
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

pub const IFCONFIG: &str = "/sbin/ifconfig";

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct RecordingRunner {
        pub calls: RefCell<Vec<(String, Vec<String>)>>,
        pub outputs: RefCell<Vec<(String, Result<String, String>)>>,
    }

    impl RecordingRunner {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn push_output(&self, needle: &str, result: Result<String, String>) {
            self.outputs.borrow_mut().push((needle.to_string(), result));
        }

        pub fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.borrow().clone()
        }

        pub fn joined(&self) -> Vec<String> {
            self.calls
                .borrow()
                .iter()
                .map(|(program, args)| format!("{program} {}", args.join(" ")))
                .collect()
        }
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
            self.calls.borrow_mut().push((
                program.to_string(),
                args.iter().map(|arg| arg.to_string()).collect(),
            ));
            let joined = format!("{program} {}", args.join(" "));
            let outputs = self.outputs.borrow();
            for (needle, result) in outputs.iter() {
                if joined.contains(needle) {
                    return result.clone();
                }
            }
            Ok(String::new())
        }
    }
}
