//! The `jit-runner` executable the spec examples run through.

use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cargo_metadata::Message;

use crate::tools;

/// Builds `jit-runner` in the workspace at `root` and returns its path.
pub fn build(root: &Path) -> Result<PathBuf, String> {
    let output = Command::new(tools::cargo())
        .args([
            "build",
            "-p",
            "mora-testkit",
            "--bin",
            "jit-runner",
            "--locked",
            "--message-format=json-render-diagnostics",
        ])
        .current_dir(root)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|err| format!("cannot run cargo: {err}"))?;
    if !output.status.success() {
        return Err(format!("building jit-runner failed: {}", output.status));
    }
    executable(&output.stdout)
        .ok_or_else(|| "cargo did not report the jit-runner executable".into())
}

/// The executable of the `compiler-artifact` message for `jit-runner`, in
/// cargo's JSON `messages`.
pub fn executable(messages: &[u8]) -> Option<PathBuf> {
    Message::parse_stream(BufReader::new(Cursor::new(messages)))
        .filter_map(Result::ok)
        .find_map(|message| match message {
            Message::CompilerArtifact(artifact) if artifact.target.name == "jit-runner" => {
                artifact.executable.map(Into::into)
            }
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(name: &str, executable: &str) -> String {
        format!(
            r#"{{"reason":"compiler-artifact","package_id":"path+file:///w/{name}#0.1.0","manifest_path":"/w/{name}/Cargo.toml","target":{{"kind":["bin"],"crate_types":["bin"],"name":"{name}","src_path":"/w/{name}/src/main.rs","edition":"2024","doc":true,"doctest":false,"test":false}},"profile":{{"opt_level":"0","debuginfo":2,"debug_assertions":true,"overflow_checks":true,"test":false}},"features":[],"filenames":["{executable}"],"executable":"{executable}","fresh":true}}"#
        )
    }

    #[test]
    fn executable_comes_from_the_runner_artifact() {
        let messages = format!(
            "{}\n{}\n",
            artifact("other", "/t/debug/other"),
            artifact("jit-runner", "/t/debug/jit-runner")
        );
        assert_eq!(
            executable(messages.as_bytes()),
            Some(PathBuf::from("/t/debug/jit-runner"))
        );
        let none = format!("{}\n", artifact("other", "/t/debug/other"));
        assert_eq!(executable(none.as_bytes()), None);
    }
}
