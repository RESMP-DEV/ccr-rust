// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use super::pyright_workspace::PrivateWorkspaceRoot;
use super::{NativeTool, ToolResult};
use crate::mcp::protocol::McpTool;

const MAX_FILES_PER_REQUEST: usize = 50;
const PYRIGHT_TIMEOUT_SECS: u64 = 60;

pub struct PyrightTool {
    project_root: PathBuf,
    workspaces: PrivateWorkspaceRoot,
    semaphore: Arc<Semaphore>,
}

#[derive(Debug, Deserialize)]
struct FileInput {
    path: String,
    content: Option<String>,
}

#[derive(Debug, Serialize)]
struct Diagnostic {
    file: String,
    line: usize,
    column: usize,
    severity: String,
    message: String,
    rule: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct PyrightOutput {
    #[serde(default)]
    general_diagnostics: Vec<PyrightDiagnostic>,
    #[serde(default)]
    summary: PyrightSummary,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct PyrightSummary {
    #[serde(default)]
    error_count: usize,
    #[serde(default)]
    warning_count: usize,
    #[serde(default)]
    information_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PyrightDiagnostic {
    #[serde(default)]
    file: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    rule: String,
    #[serde(default)]
    range: Option<PyrightRange>,
}

#[derive(Debug, Deserialize)]
struct PyrightRange {
    start: PyrightPosition,
}

#[derive(Debug, Deserialize)]
struct PyrightPosition {
    line: usize,
    character: usize,
}

impl PyrightTool {
    pub fn new(
        project_root: PathBuf,
        workspace_dir: PathBuf,
        max_concurrent: usize,
    ) -> Result<Self> {
        let workspaces = PrivateWorkspaceRoot::prepare(workspace_dir)?;
        Ok(Self {
            project_root,
            workspaces,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        })
    }

    async fn type_check(&self, args: Value) -> Result<ToolResult> {
        debug!("pyright type_check called");

        let files: Vec<FileInput> = args
            .get("files")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .context("missing required field: files")?;

        if files.is_empty() {
            return Ok(ToolResult::text(
                json!({ "diagnostics": [], "summary": "no files to check" }).to_string(),
            ));
        }

        if files.len() > MAX_FILES_PER_REQUEST {
            return Ok(ToolResult::error(format!(
                "too many files: {} (max {MAX_FILES_PER_REQUEST})",
                files.len()
            )));
        }

        for f in &files {
            let p = Path::new(&f.path);
            if p.is_absolute() || p.components().any(|c| c == std::path::Component::ParentDir) {
                return Ok(ToolResult::error(format!(
                    "path must be relative and not contain '..': {}",
                    f.path
                )));
            }
        }

        let _permit = self.semaphore.acquire().await.context("semaphore closed")?;

        let ws_path = self.workspaces.create_request_directory()?;

        let result = self.run_in_workspace(&ws_path, &files).await;

        if let Err(e) = std::fs::remove_dir_all(&ws_path) {
            warn!(path = %ws_path.display(), "workspace cleanup failed: {e}");
        }

        result
    }

    async fn run_in_workspace(&self, ws_path: &Path, files: &[FileInput]) -> Result<ToolResult> {
        if let Err(e) = self.prepare_workspace(ws_path, files) {
            return Ok(ToolResult::error(format!("workspace setup failed: {e}")));
        }

        match self.run_pyright(ws_path).await {
            Ok(output) => {
                let diagnostics = self.parse_output(&output, ws_path);
                let summary = format!(
                    "{} error(s), {} warning(s), {} info across {} file(s)",
                    diagnostics.iter().filter(|d| d.severity == "error").count(),
                    diagnostics
                        .iter()
                        .filter(|d| d.severity == "warning")
                        .count(),
                    diagnostics
                        .iter()
                        .filter(|d| d.severity == "information")
                        .count(),
                    files.len(),
                );
                info!(
                    diag_count = diagnostics.len(),
                    file_count = files.len(),
                    "pyright type_check completed"
                );
                Ok(ToolResult::text(
                    json!({ "diagnostics": diagnostics, "summary": summary }).to_string(),
                ))
            }
            Err(e) => Ok(ToolResult::error(format!("pyright execution failed: {e}"))),
        }
    }

    fn prepare_workspace(&self, ws_path: &Path, files: &[FileInput]) -> Result<()> {
        let pyright_config = self.project_root.join("pyrightconfig.json");
        if pyright_config.exists() {
            std::os::unix::fs::symlink(&pyright_config, ws_path.join("pyrightconfig.json"))
                .context("failed to symlink pyrightconfig.json")?;

            if let Ok(config_text) = std::fs::read_to_string(&pyright_config) {
                if let Ok(config) = serde_json::from_str::<Value>(&config_text) {
                    if let Some(includes) = config.get("include").and_then(|v| v.as_array()) {
                        for inc in includes {
                            if let Some(dir_name) = inc.as_str() {
                                let include_path = Path::new(dir_name);
                                if include_path.as_os_str().is_empty()
                                    || include_path == Path::new(".")
                                {
                                    self.symlink_project_root_entries(ws_path)?;
                                } else if let Some(root) = top_level_component(include_path) {
                                    self.symlink_project_entry(ws_path, root)?;
                                }
                            }
                        }
                    }
                }
            }
        }

        for f in files {
            let rel = Path::new(&f.path);
            if let Some(root) = top_level_component(rel) {
                self.symlink_project_entry(ws_path, root)?;
            }
        }

        // Overlay modified files: break symlinks where needed and write content
        for f in files {
            let Some(ref content) = f.content else {
                continue;
            };
            let rel = Path::new(&f.path);
            let target = ws_path.join(rel);

            // If this path exists via a directory symlink, we need to break
            // the chain: remove the top-level dir symlink, recreate it as a
            // real directory tree, and then write the overlay file.
            if target.exists() || target.symlink_metadata().is_ok() {
                self.break_symlink_for_overlay(ws_path, rel)?;
            } else if let Some(parent) = rel.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(ws_path.join(parent))
                        .with_context(|| format!("failed to create dir: {}", parent.display()))?;
                }
            }

            std::fs::write(&target, content)
                .with_context(|| format!("failed to write overlay: {}", f.path))?;
        }

        Ok(())
    }

    fn symlink_project_root_entries(&self, ws_path: &Path) -> Result<()> {
        for entry in std::fs::read_dir(&self.project_root).with_context(|| {
            format!(
                "failed to read project root: {}",
                self.project_root.display()
            )
        })? {
            let entry = entry?;
            let name = entry.file_name();
            if name == ".git" {
                continue;
            }
            self.symlink_project_entry(ws_path, Path::new(&name))?;
        }
        Ok(())
    }

    fn symlink_project_entry(&self, ws_path: &Path, rel: &Path) -> Result<()> {
        let src = self.project_root.join(rel);
        if !src.exists() {
            return Ok(());
        }

        let dst = ws_path.join(rel);
        if dst.symlink_metadata().is_ok() {
            return Ok(());
        }

        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create dir: {}", parent.display()))?;
        }

        std::os::unix::fs::symlink(&src, &dst)
            .with_context(|| format!("failed to symlink {} to {}", src.display(), dst.display()))?;
        Ok(())
    }

    fn break_symlink_for_overlay(&self, ws_path: &Path, rel: &Path) -> Result<()> {
        // Walk from the workspace root down the path components to find the
        // first symlink in the chain. Replace it with a real directory that
        // mirrors the source via symlinks, except for the subtree we need to
        // overlay.
        let mut accumulated = PathBuf::new();
        for component in rel.parent().into_iter().flat_map(|p| p.components()) {
            accumulated.push(component);
            let ws_entry = ws_path.join(&accumulated);
            let meta = match ws_entry.symlink_metadata() {
                Ok(m) => m,
                Err(_) => {
                    std::fs::create_dir_all(&ws_entry).ok();
                    continue;
                }
            };
            if meta.is_symlink() {
                // This is a symlinked directory — replace it with a real dir
                // that contains symlinks to all its children.
                let link_target = std::fs::read_link(&ws_entry)
                    .with_context(|| format!("failed to read symlink: {}", ws_entry.display()))?;
                std::fs::remove_file(&ws_entry)
                    .with_context(|| format!("failed to remove symlink: {}", ws_entry.display()))?;
                std::fs::create_dir_all(&ws_entry)
                    .with_context(|| format!("failed to create dir: {}", ws_entry.display()))?;

                // Populate with symlinks to each child of the original directory
                if let Ok(entries) = std::fs::read_dir(&link_target) {
                    for entry in entries.flatten() {
                        let child_name = entry.file_name();
                        let child_dst = ws_entry.join(&child_name);
                        if !child_dst.exists() {
                            std::os::unix::fs::symlink(entry.path(), &child_dst).ok();
                        }
                    }
                }
            }
        }

        // Remove the file-level symlink if it exists
        let target_file = ws_path.join(rel);
        if target_file.symlink_metadata().is_ok() {
            std::fs::remove_file(&target_file).ok();
        }

        Ok(())
    }

    async fn run_pyright(&self, ws_path: &Path) -> Result<String> {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(PYRIGHT_TIMEOUT_SECS),
            tokio::process::Command::new("pyright")
                .arg("--outputjson")
                .arg("--project")
                .arg(ws_path)
                .env_remove("CCR_MCP_AUTH_TOKEN")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output(),
        )
        .await
        .context("pyright timed out")?
        .context("failed to spawn pyright")?;

        // Pyright returns non-zero on diagnostics — that's expected
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if stdout.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                warn!(stderr = %stderr, "pyright produced no stdout");
            }
            anyhow::bail!("pyright produced no output (exit code: {})", output.status);
        }

        Ok(stdout)
    }

    fn parse_output(&self, raw: &str, ws_path: &Path) -> Vec<Diagnostic> {
        let parsed: PyrightOutput = match serde_json::from_str(raw) {
            Ok(o) => o,
            Err(e) => {
                warn!("failed to parse pyright output: {e}");
                return vec![];
            }
        };

        parsed
            .general_diagnostics
            .into_iter()
            .map(|d| {
                let file = d
                    .file
                    .strip_prefix(ws_path.to_string_lossy().as_ref())
                    .unwrap_or(&d.file)
                    .trim_start_matches('/')
                    .to_string();
                let (line, column) = d
                    .range
                    .map(|r| (r.start.line + 1, r.start.character + 1))
                    .unwrap_or((0, 0));
                Diagnostic {
                    file,
                    line,
                    column,
                    severity: d.severity.to_lowercase(),
                    message: d.message,
                    rule: d.rule,
                }
            })
            .collect()
    }
}

fn top_level_component(path: &Path) -> Option<&Path> {
    path.components().find_map(|component| match component {
        std::path::Component::Normal(name) => Some(Path::new(name)),
        _ => None,
    })
}

#[async_trait]
impl NativeTool for PyrightTool {
    fn tools(&self) -> Vec<McpTool> {
        vec![McpTool {
            name: "type_check".to_string(),
            description: "Run Pyright type-checking on Python files. \
                Send file paths (relative to project root) and optionally file contents \
                for modified files. Returns structured diagnostics."
                .to_string(),
            inputSchema: json!({
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Relative path from project root (e.g. 'alphaheng/worker/agent.py')"
                                },
                                "content": {
                                    "type": "string",
                                    "description": "File content. Omit to check the hub's copy."
                                }
                            },
                            "required": ["path"],
                            "additionalProperties": false
                        },
                        "description": "Files to type-check (max 50)."
                    }
                },
                "required": ["files"],
                "additionalProperties": false
            }),
        }]
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<ToolResult> {
        match name {
            "type_check" => self.type_check(arguments).await,
            _ => Ok(ToolResult::error(format!("unknown pyright tool: {name}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pyright_json() {
        let raw = r#"{
            "version": "1.1.390",
            "generalDiagnostics": [
                {
                    "file": "/tmp/ws/alphaheng/worker/agent.py",
                    "severity": "error",
                    "message": "Type of \"x\" is \"Unknown\"",
                    "rule": "reportUnknownVariableType",
                    "range": {
                        "start": { "line": 10, "character": 4 },
                        "end": { "line": 10, "character": 5 }
                    }
                }
            ],
            "summary": {
                "errorCount": 1,
                "warningCount": 0,
                "informationCount": 0
            }
        }"#;

        let scratch = tempfile::tempdir().unwrap();
        let tool = PyrightTool::new(
            PathBuf::from("/project"),
            scratch.path().join("workspaces"),
            1,
        )
        .unwrap();
        let diagnostics = tool.parse_output(raw, Path::new("/tmp/ws"));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].file, "alphaheng/worker/agent.py");
        assert_eq!(diagnostics[0].line, 11);
        assert_eq!(diagnostics[0].column, 5);
        assert_eq!(diagnostics[0].severity, "error");
        assert_eq!(diagnostics[0].rule, "reportUnknownVariableType");
    }

    #[test]
    fn rejects_absolute_paths() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let tool = PyrightTool::new(
            PathBuf::from("/project"),
            scratch.path().join("workspaces"),
            1,
        )
        .unwrap();
        let result = rt.block_on(tool.type_check(json!({
            "files": [{ "path": "/etc/passwd" }]
        })));
        let result = result.unwrap();
        assert!(result.is_error);
    }

    #[test]
    fn rejects_parent_traversal() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let tool = PyrightTool::new(
            PathBuf::from("/project"),
            scratch.path().join("workspaces"),
            1,
        )
        .unwrap();
        let result = rt.block_on(tool.type_check(json!({
            "files": [{ "path": "../../etc/passwd" }]
        })));
        let result = result.unwrap();
        assert!(result.is_error);
    }

    #[test]
    fn prepare_workspace_preserves_sibling_modules_for_nested_overlay() {
        let project = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();

        std::fs::create_dir(project.path().join("pkg")).unwrap();
        std::fs::write(
            project.path().join("pyrightconfig.json"),
            r#"{"include":["."]}"#,
        )
        .unwrap();
        std::fs::write(
            project.path().join("pkg/foo.py"),
            "from .bar import value\n",
        )
        .unwrap();
        std::fs::write(project.path().join("pkg/bar.py"), "value: int = 1\n").unwrap();

        let tool = PyrightTool::new(
            project.path().to_path_buf(),
            scratch.path().join("workspaces"),
            1,
        )
        .unwrap();
        tool.prepare_workspace(
            ws.path(),
            &[FileInput {
                path: "pkg/foo.py".to_string(),
                content: Some("from .bar import value\nreveal_type(value)\n".to_string()),
            }],
        )
        .unwrap();

        assert!(!std::fs::symlink_metadata(ws.path().join("pkg"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(ws.path().join("pkg/bar.py").exists());
        assert_eq!(
            std::fs::read_to_string(ws.path().join("pkg/foo.py")).unwrap(),
            "from .bar import value\nreveal_type(value)\n"
        );
    }
}
