// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plugin catalog resolution.
//!
//! The effective plugin set is the merge of three sources, in increasing
//! precedence: the drop-in plugin directory (every executable regular file,
//! named after its file stem), config declarations in `Plugins.plugins`
//! (which override a same-named drop-in), and the built-in transformer
//! names (which always win over any plugin).
//!
//! The same resolution feeds transformer registration and the
//! `ccr-rust plugins` listing, so what the CLI shows is what the router
//! registers.

use crate::config::{PluginOnError, PluginSpec, PluginStages, PluginsConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// File suffixes never treated as drop-in plugins even when executable.
const SKIP_SUFFIXES: &[&str] = &[".bak", ".tmp", ".swp", "~"];

/// Where a resolved plugin came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSource {
    /// Compiled into the binary.
    Builtin,
    /// Declared in `Plugins.plugins`.
    Config,
    /// Discovered in the drop-in plugin directory.
    Directory,
}

impl PluginSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Config => "config",
            Self::Directory => "directory",
        }
    }
}

/// One plugin resolved for registration or listing.
#[derive(Debug, Clone)]
pub struct ResolvedPlugin {
    pub name: String,
    pub source: PluginSource,
    pub spec: Arc<PluginSpec>,
}

/// Scan `dir` for drop-in plugins: executable regular files whose stem
/// becomes the plugin name. A missing or unreadable directory is not an
/// error (the default path is a convention, not a requirement).
fn scan_plugin_dir(dir: &Path) -> Vec<(String, PathBuf)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            if dir.exists() {
                warn!(dir = %dir.display(), error = %error, "cannot read plugin directory");
            } else {
                debug!(dir = %dir.display(), "plugin directory absent, no drop-in plugins");
            }
            return Vec::new();
        }
    };

    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name.starts_with('.')
            || SKIP_SUFFIXES
                .iter()
                .any(|suffix| file_name.ends_with(suffix))
        {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() || !is_executable(&metadata) {
            debug!(path = %path.display(), "skipping non-executable plugin directory entry");
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if stem.is_empty() {
            continue;
        }
        found.push((stem.to_string(), path));
    }

    // `a.sh` and `a.py` would both claim the stem `a`; keep the
    // lexicographically first path so the winner is deterministic.
    found.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    found.dedup_by(|a, b| {
        if a.0 == b.0 {
            warn!(
                name = %a.0,
                kept = %b.1.display(),
                dropped = %a.1.display(),
                "duplicate plugin stem in plugin directory; keeping the first path"
            );
            true
        } else {
            false
        }
    });
    found
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    !metadata.permissions().readonly()
}

/// Resolve the effective plugin set for a registry whose built-in transformer
/// names are `builtin_names`. Config declarations override same-named
/// drop-ins; any plugin shadowing a built-in name is skipped with a warning.
///
/// The result is sorted by name so registration order and CLI output are
/// deterministic.
pub fn resolve_plugins(config: &PluginsConfig, builtin_names: &[&str]) -> Vec<ResolvedPlugin> {
    let mut resolved: Vec<ResolvedPlugin> = Vec::new();

    for (name, path) in scan_plugin_dir(&config.resolved_dir()) {
        let spec = PluginSpec {
            command: path.to_string_lossy().into_owned(),
            args: Vec::new(),
            timeout_ms: PluginSpec::DEFAULT_TIMEOUT_MS,
            env: Default::default(),
            stages: PluginStages::Both,
            on_error: PluginOnError::Passthrough,
        };
        resolved.push(ResolvedPlugin {
            name,
            source: PluginSource::Directory,
            spec: Arc::new(spec),
        });
    }

    for (name, spec) in &config.plugins {
        if let Some(existing) = resolved.iter_mut().find(|plugin| plugin.name == *name) {
            info!(
                name = %name,
                command = %spec.command,
                overrides = %existing.spec.command,
                "config plugin declaration overrides drop-in file"
            );
            existing.source = PluginSource::Config;
            existing.spec = Arc::new(spec.clone());
        } else {
            resolved.push(ResolvedPlugin {
                name: name.clone(),
                source: PluginSource::Config,
                spec: Arc::new(spec.clone()),
            });
        }
    }

    resolved.retain(|plugin| {
        if builtin_names.contains(&plugin.name.as_str()) {
            warn!(
                name = %plugin.name,
                "plugin shadows a built-in transformer; built-in wins, plugin skipped"
            );
            false
        } else {
            true
        }
    });

    resolved.sort_by(|a, b| a.name.cmp(&b.name));
    resolved
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    fn exec_file(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\ncat\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn config_with(dir: Option<String>, plugins: serde_json::Value) -> PluginsConfig {
        let mut value = json!({});
        if let Some(dir) = dir {
            value["dir"] = json!(dir);
        }
        value["plugins"] = plugins;
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn scans_executable_files_by_stem() {
        let dir = tempfile::tempdir().unwrap();
        exec_file(dir.path(), "redact-pii.sh");
        exec_file(dir.path(), "clamp.py");

        let config = config_with(Some(dir.path().to_string_lossy().into_owned()), json!({}));
        let resolved = resolve_plugins(&config, &[]);
        let names: Vec<&str> = resolved.iter().map(|plugin| plugin.name.as_str()).collect();
        assert_eq!(names, vec!["clamp", "redact-pii"], "sorted by name");
        assert!(resolved
            .iter()
            .all(|plugin| plugin.source == PluginSource::Directory));
    }

    #[test]
    fn skips_hidden_backup_and_non_executable_entries() {
        let dir = tempfile::tempdir().unwrap();
        exec_file(dir.path(), "good.sh");
        std::fs::write(dir.path().join("README.md"), "docs").unwrap();
        std::fs::write(dir.path().join(".hidden.sh"), "#!/bin/sh").unwrap();
        std::fs::write(dir.path().join("old.sh.bak"), "#!/bin/sh").unwrap();
        let non_exec = dir.path().join("notexec.sh");
        std::fs::write(&non_exec, "#!/bin/sh").unwrap();
        std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o644)).unwrap();

        let config = config_with(Some(dir.path().to_string_lossy().into_owned()), json!({}));
        let resolved = resolve_plugins(&config, &[]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "good");
    }

    #[test]
    fn same_stem_collision_takes_first_path() {
        let dir = tempfile::tempdir().unwrap();
        exec_file(dir.path(), "dup.sh");
        exec_file(dir.path(), "dup.py");

        let config = config_with(Some(dir.path().to_string_lossy().into_owned()), json!({}));
        let resolved = resolve_plugins(&config, &[]);
        assert_eq!(resolved.len(), 1, "duplicate stems collapse to one plugin");
    }

    #[test]
    fn missing_directory_is_not_an_error() {
        let config = config_with(Some("/nonexistent/plugin/dir".to_string()), json!({}));
        assert!(resolve_plugins(&config, &[]).is_empty());
    }

    #[test]
    fn config_declaration_overrides_drop_in() {
        let dir = tempfile::tempdir().unwrap();
        exec_file(dir.path(), "redact.sh");

        let config = config_with(
            Some(dir.path().to_string_lossy().into_owned()),
            json!({ "redact": { "command": "/bin/cat", "timeout_ms": 500 } }),
        );
        let resolved = resolve_plugins(&config, &[]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].source, PluginSource::Config);
        assert_eq!(resolved[0].spec.command, "/bin/cat");
        assert_eq!(resolved[0].spec.timeout_ms, 500);
    }

    #[test]
    fn builtins_win_over_plugins() {
        let dir = tempfile::tempdir().unwrap();
        exec_file(dir.path(), "anthropic.sh");

        let config = config_with(
            Some(dir.path().to_string_lossy().into_owned()),
            json!({ "maxtoken": { "command": "/bin/cat" } }),
        );
        let resolved = resolve_plugins(&config, &["anthropic", "maxtoken"]);
        assert!(resolved.is_empty(), "built-in names must never be shadowed");
    }

    #[test]
    fn default_dir_used_when_not_configured() {
        let config = PluginsConfig::default();
        assert!(resolve_plugins(&config, &[]).is_empty());
    }
}
