// SPDX-License-Identifier: AGPL-3.0-or-later
//! External transformer plugin configuration (`Plugins` section).
//!
//! Plugins restore the Node.js router's runtime extensibility on the Rust
//! router: an operator declares executable programs that transform request
//! or response payloads without recompiling CCR-Rust. See docs/plugins.md
//! for the wire protocol and examples/plugins/ for ready-to-use scripts.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Directory scanned for drop-in plugins when `Plugins.dir` is not set.
pub const DEFAULT_PLUGINS_DIR: &str = ".claude-code-router/plugins";

/// Which stages of a request/response cycle a plugin participates in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PluginStages {
    /// Transform requests only; responses pass through without spawning.
    Request,
    /// Transform responses only; requests pass through without spawning.
    Response,
    /// Transform both stages (default).
    #[default]
    Both,
}

impl PluginStages {
    /// Lowercase config spelling, also used by CLI output.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
            Self::Both => "both",
        }
    }
}

/// Behavior when a plugin invocation fails (non-zero exit, invalid output,
/// spawn error, or timeout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PluginOnError {
    /// Log a warning and pass the payload through unchanged (default).
    ///
    /// The default favors availability: an external process is the fragile
    /// layer, and silently skipping an optional transform is strictly less
    /// harmful than failing every request on the tier that uses it. Every
    /// failure is logged at warn level with the plugin name and stderr tail.
    #[default]
    Passthrough,
    /// Propagate the failure: the request is rejected with an error.
    ///
    /// Use for correctness-critical plugins (for example PII redaction)
    /// where passing through untransformed data would be worse than failing.
    Fail,
}

impl PluginOnError {
    /// Lowercase config spelling, also used by CLI output.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Passthrough => "passthrough",
            Self::Fail => "fail",
        }
    }
}

/// One external transformer plugin declaration.
///
/// The plugin process receives the JSON payload on stdin and must write the
/// transformed JSON payload on stdout, exiting 0. The invocation context is
/// exposed through the `CCR_STAGE` (`request` or `response`) and
/// `CCR_PLUGIN_NAME` environment variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSpec {
    /// Program to run. Bare names resolve through `PATH`; paths containing a
    /// separator are used as-is. Never executed through a shell.
    pub command: String,

    /// Static arguments passed to every invocation.
    #[serde(default)]
    pub args: Vec<String>,

    /// Kill the plugin after this many milliseconds (default 10000).
    #[serde(default = "default_plugin_timeout_ms")]
    pub timeout_ms: u64,

    /// Extra environment variables layered on top of the inherited process
    /// environment for every invocation.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Stages the plugin participates in (default "both").
    #[serde(default)]
    pub stages: PluginStages,

    /// Failure behavior (default "passthrough").
    #[serde(default)]
    pub on_error: PluginOnError,
}

fn default_plugin_timeout_ms() -> u64 {
    PluginSpec::DEFAULT_TIMEOUT_MS
}

impl PluginSpec {
    /// Per-invocation timeout applied when `timeout_ms` is not configured.
    pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

    /// Validate one declaration. Called from `Config::from_file` so every
    /// entry point (start, validate, doctor) fails fast on a broken plugin
    /// instead of degrading per request.
    pub fn validate(&self, name: &str) -> Result<()> {
        if name.trim().is_empty() {
            anyhow::bail!("Plugins.plugins entry name must be nonblank");
        }
        if self.command.trim().is_empty() {
            anyhow::bail!("Plugins.plugins.{name}.command must be nonblank");
        }
        if self.timeout_ms == 0 {
            anyhow::bail!("Plugins.plugins.{name}.timeout_ms must be greater than 0");
        }
        check_command_available(name, &self.command, &self.env)
    }
}

/// Whether `path` is a regular file that invocation could actually start.
/// A bare command is only accepted when the candidate is executable, matching
/// the explicit-path branch instead of trusting `is_file()` alone.
fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Verify a plugin command resolves. Absolute and relative paths must exist
/// and be executable; bare names must resolve to an executable file on the
/// same `PATH` the spawned plugin uses.
fn check_command_available(name: &str, command: &str, env: &HashMap<String, String>) -> Result<()> {
    if command.contains(std::path::MAIN_SEPARATOR) || command.contains('/') {
        let path = PathBuf::from(command);
        let meta = std::fs::metadata(&path).with_context(|| {
            format!(
                "Plugins.plugins.{name}: command '{}' does not exist",
                command
            )
        })?;
        if !meta.is_file() {
            anyhow::bail!(
                "Plugins.plugins.{name}: command '{}' is not a regular file",
                command
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o111 == 0 {
                anyhow::bail!(
                    "Plugins.plugins.{name}: command '{}' is not executable",
                    command
                );
            }
        }
        return Ok(());
    }
    // Invocation resolves a bare command against the declared `env.PATH` when
    // the plugin sets one, so validation must use the same search path or it
    // rejects declarations that would in fact start.
    let path_var = match env.get("PATH") {
        Some(declared) => declared.clone(),
        None => std::env::var("PATH").unwrap_or_default(),
    };
    let found = std::env::split_paths(&path_var).any(|dir| is_executable_file(&dir.join(command)));
    if !found {
        anyhow::bail!(
            "Plugins.plugins.{name}: command '{}' was not found on PATH",
            command
        );
    }
    Ok(())
}

/// Top-level `Plugins` configuration section.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginsConfig {
    /// Directory scanned for drop-in plugins. Every executable regular file
    /// becomes a plugin named after its file stem. Defaults to
    /// `~/.claude-code-router/plugins`; a missing directory is not an error.
    #[serde(default, rename = "dir")]
    pub dir: Option<String>,

    /// Named plugin declarations keyed by the name used in transformer
    /// `use` lists. Declarations override a same-named drop-in file;
    /// built-in transformer names always win.
    #[serde(default, rename = "plugins")]
    pub plugins: HashMap<String, PluginSpec>,
}

impl PluginsConfig {
    /// Whether the section carries no configuration at all.
    pub fn is_empty(&self) -> bool {
        self.dir.is_none() && self.plugins.is_empty()
    }

    /// The configured plugin directory with `~` expanded, or the default
    /// `~/.claude-code-router/plugins`.
    pub fn resolved_dir(&self) -> PathBuf {
        let raw = self
            .dir
            .clone()
            .unwrap_or_else(|| format!("~/{DEFAULT_PLUGINS_DIR}"));
        PathBuf::from(shellexpand::tilde(&raw).into_owned())
    }

    /// Validate the section. Called from `Config::from_file` so every entry
    /// point applies the same contract.
    pub fn validate(&self) -> Result<()> {
        for (name, spec) in &self.plugins {
            spec.validate(name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(command: &str) -> PluginSpec {
        serde_json::from_value(json!({ "command": command })).unwrap()
    }

    #[test]
    fn defaults_apply() {
        let parsed: PluginSpec = serde_json::from_value(json!({ "command": "/bin/cat" })).unwrap();
        assert_eq!(parsed.timeout_ms, 10_000);
        assert_eq!(parsed.stages, PluginStages::Both);
        assert_eq!(parsed.on_error, PluginOnError::Passthrough);
        assert!(parsed.args.is_empty());
        assert!(parsed.env.is_empty());
    }

    #[test]
    fn bare_command_resolves_against_the_declared_env_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("declared-only");
        std::fs::write(&bin, "#!/bin/sh\ncat\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut parsed = spec("declared-only");
        parsed.env.insert(
            "PATH".to_string(),
            dir.path().to_string_lossy().into_owned(),
        );
        // Validation must search the declared PATH, not the inherited one.
        assert!(parsed.validate("p").is_ok());
    }

    #[test]
    fn bare_command_rejects_a_non_executable_file_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("not-executable");
        std::fs::write(&bin, "#!/bin/sh\ncat\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let mut parsed = spec("not-executable");
        parsed.env.insert(
            "PATH".to_string(),
            dir.path().to_string_lossy().into_owned(),
        );
        let error = parsed.validate("p").unwrap_err().to_string();
        assert!(error.contains("PATH"), "unexpected error: {error}");
    }

    #[test]
    fn explicit_fields_parse() {
        let parsed: PluginSpec = serde_json::from_value(json!({
            "command": "/usr/bin/jq",
            "args": ["-c", "."],
            "timeout_ms": 250,
            "env": { "FOO": "bar" },
            "stages": "request",
            "on_error": "fail"
        }))
        .unwrap();
        assert_eq!(parsed.args, vec!["-c", "."]);
        assert_eq!(parsed.timeout_ms, 250);
        assert_eq!(parsed.stages, PluginStages::Request);
        assert_eq!(parsed.on_error, PluginOnError::Fail);
        assert_eq!(parsed.env.get("FOO").map(String::as_str), Some("bar"));
    }

    #[test]
    fn plugins_section_shape() {
        let parsed: PluginsConfig = serde_json::from_value(json!({
            "dir": "/tmp/plugins",
            "plugins": { "redact": { "command": "/usr/bin/jq" } }
        }))
        .unwrap();
        assert_eq!(parsed.dir.as_deref(), Some("/tmp/plugins"));
        assert!(parsed.plugins.contains_key("redact"));
        assert!(!parsed.is_empty());
    }

    #[test]
    fn resolved_dir_expands_tilde() {
        let config = PluginsConfig::default();
        assert!(config.resolved_dir().ends_with(DEFAULT_PLUGINS_DIR));
        let config: PluginsConfig =
            serde_json::from_value(json!({ "dir": "~/my-plugins" })).unwrap();
        assert!(config.resolved_dir().ends_with("my-plugins"));
    }

    #[test]
    fn validate_rejects_blank_command() {
        let spec = spec("   ");
        let error = spec.validate("redact").unwrap_err().to_string();
        assert!(error.contains("nonblank"), "unexpected error: {error}");
    }

    #[test]
    fn validate_rejects_zero_timeout() {
        let parsed: PluginSpec =
            serde_json::from_value(json!({ "command": "/bin/cat", "timeout_ms": 0 })).unwrap();
        let error = parsed.validate("redact").unwrap_err().to_string();
        assert!(error.contains("timeout_ms"), "unexpected error: {error}");
    }

    #[test]
    fn validate_rejects_missing_path_command() {
        let spec = spec("/nonexistent/path/to/plugin-binary");
        let error = spec.validate("redact").unwrap_err().to_string();
        assert!(
            error.contains("redact"),
            "error must name the plugin: {error}"
        );
        assert!(
            error.contains("does not exist"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn validate_accepts_system_binary_on_path() {
        let spec = spec("sh");
        spec.validate("sh-plugin").unwrap();
    }

    #[test]
    fn validate_rejects_path_command_without_exec_bit() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("not-executable");
        std::fs::write(&script, "#!/bin/sh\ncat\n").unwrap();
        let spec = spec(script.to_str().unwrap());
        let error = spec.validate("redact").unwrap_err().to_string();
        assert!(
            error.contains("not executable"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn validate_accepts_executable_path_command() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("executable");
        std::fs::write(&script, "#!/bin/sh\ncat\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        spec(script.to_str().unwrap()).validate("ok").unwrap();
    }
}
