// SPDX-License-Identifier: AGPL-3.0-or-later
//! External transformer plugin runtime.
//!
//! A plugin is an executable program: it receives one JSON document (the
//! request or the response) on stdin, writes the transformed JSON document
//! on stdout, and exits 0. The invocation context is exposed through the
//! `CCR_STAGE` and `CCR_PLUGIN_NAME` environment variables. The program is
//! spawned directly (never through a shell), bounded by a per-invocation
//! timeout, and its stderr is captured for diagnostics.
//!
//! Wire protocol and configuration live in docs/plugins.md; catalog
//! resolution (config declarations plus the drop-in directory) lives in
//! `super::plugin_catalog`.

use super::Transformer;
use crate::config::{PluginOnError, PluginSpec, PluginStages};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Maximum stderr text retained for failure diagnostics. Plugin stderr is
/// normally empty; a chatty plugin cannot balloon a log line past this cap.
const STDERR_TAIL_BYTES: usize = 4096;

/// Maximum plugin stdout retained. A plugin that streams past this is
/// terminated instead of growing the router's memory without bound.
const MAX_STDOUT_BYTES: usize = 8 * 1024 * 1024;

/// Maximum plugin stderr retained. The pipe is drained in full, but only the
/// tail is kept so a diagnostic flood cannot accumulate.
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// Captured plugin stdout, or the fact that it exceeded the retention limit.
enum StdoutCapture {
    Bytes(Vec<u8>),
    TooLarge,
}

/// Read at most `limit` bytes, reporting an overflow instead of buffering the
/// whole stream. Reading stops on overflow, which leaves the plugin blocked on
/// a full pipe until the invocation is torn down.
fn read_limited(reader: &mut impl Read, limit: usize) -> StdoutCapture {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if buffer.len() + read > limit {
                    return StdoutCapture::TooLarge;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
        }
    }
    StdoutCapture::Bytes(buffer)
}

/// Drain the whole pipe while retaining only its final `limit` bytes.
fn read_tail(reader: &mut impl Read, limit: usize) -> Vec<u8> {
    let mut tail: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                tail.extend_from_slice(&chunk[..read]);
                if tail.len() > limit {
                    let excess = tail.len() - limit;
                    tail.drain(..excess);
                }
            }
        }
    }
    tail
}

/// Wait for a worker thread's result over its channel until `deadline`. On
/// expiry the worker is left detached and the caller gets `None`. Every pipe
/// wait shares one deadline, so a stalled plugin costs at most its own timeout.
fn wait_until<T>(receiver: &std::sync::mpsc::Receiver<T>, deadline: Instant) -> Option<T> {
    let now = Instant::now();
    if now >= deadline {
        return None;
    }
    receiver.recv_timeout(deadline - now).ok()
}

/// Kill the plugin's whole process group, then reap it.
///
/// Called only while the group leader is still unreaped, so `-{pid}` cannot
/// reach a recycled process group. # Safety (unix path): `libc::kill` only
/// sends a signal; the plugin was spawned with `process_group(0)`, so the
/// negative pid is its own group and the signal cannot escape it.
#[cfg(unix)]
fn kill_plugin_group(child: &mut std::process::Child) {
    let pid = child.id() as i32;
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.wait();
}

#[cfg(not(unix))]
fn kill_plugin_group(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Whether a plugin exited because the group kill signalled it. A plugin that
/// was still running at the deadline dies by `SIGKILL`; a plugin that had
/// already exited keeps its own exit status.
#[cfg(unix)]
fn terminated_by_group_kill(status: &std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    status.signal() == Some(libc::SIGKILL)
}

#[cfg(not(unix))]
fn terminated_by_group_kill(_status: &std::process::ExitStatus) -> bool {
    false
}

/// A transformer backed by an external executable declared in `Plugins`.
pub struct ExternalTransformer {
    name: String,
    spec: Arc<PluginSpec>,
}

impl ExternalTransformer {
    pub fn new(name: String, spec: Arc<PluginSpec>) -> Self {
        Self { name, spec }
    }

    /// The resolved plugin specification backing this transformer.
    pub fn spec(&self) -> &PluginSpec {
        &self.spec
    }

    /// Run one stage: serialize, spawn, enforce the timeout, parse stdout.
    fn run_stage(&self, stage: &'static str, payload: &Value) -> Result<Value> {
        let input = serde_json::to_vec(payload)
            .with_context(|| format!("plugin '{}': serialize {stage} payload", self.name))?;

        let mut command = Command::new(&self.spec.command);
        command
            .args(&self.spec.args)
            .env("CCR_STAGE", stage)
            .env("CCR_PLUGIN_NAME", &self.name)
            .envs(&self.spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Own process group so a timed-out plugin's whole tree can be
        // signalled: orphaned grandchildren would otherwise inherit the
        // pipes and stall the output readers long past the timeout.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().with_context(|| {
            format!(
                "plugin '{}': spawn '{}' for {stage} stage",
                self.name, self.spec.command
            )
        })?;

        // Payloads routinely exceed the pipe buffer, so the stdin write runs
        // on its own thread and must drain even if the child exits early.
        // Results come back over channels so the post-exit drain can be
        // bounded instead of joining unconditionally.
        let (stdin_done_tx, stdin_done_rx) = std::sync::mpsc::channel::<()>();
        let (stdout_tx, stdout_rx) = std::sync::mpsc::channel::<StdoutCapture>();
        let (stderr_tx, stderr_rx) = std::sync::mpsc::channel::<Vec<u8>>();

        let mut stdin = child.stdin.take().expect("stdin piped");
        let mut stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");
        // The join handle is dropped on purpose: completion is observed
        // through `stdin_done_rx`, bounded by the timeout deadline.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
            let _ = stdin_done_tx.send(());
        });

        let out_reader = std::thread::spawn(move || {
            let _ = stdout_tx.send(read_limited(&mut stdout, MAX_STDOUT_BYTES));
        });
        let err_reader = std::thread::spawn(move || {
            let _ = stderr_tx.send(read_tail(&mut stderr, MAX_STDERR_BYTES));
        });

        // Drain the pipes first and only then decide whether to signal. The
        // group leader is still unreaped at that point, so `-{pid}` cannot
        // resolve to a recycled process group, and a grandchild holding a pipe
        // open is torn down before its reader threads can be left blocked.
        let deadline = Instant::now() + Duration::from_millis(self.spec.timeout_ms);
        let stdout_capture = wait_until(&stdout_rx, deadline);
        let stderr_bytes = wait_until(&stderr_rx, deadline);
        let _ = wait_until(&stdin_done_rx, deadline);

        let stdout_too_large = matches!(stdout_capture, Some(StdoutCapture::TooLarge));
        let output = match stdout_capture {
            Some(StdoutCapture::Bytes(bytes)) => Some(bytes),
            Some(StdoutCapture::TooLarge) | None => None,
        };

        // Decide before reaping. A stalled pipe means the plugin tree is
        // still alive, so the whole group is torn down while the leader is
        // still an unreaped zombie: its pid cannot be recycled, and `-{pid}`
        // therefore cannot resolve to an unrelated process group.
        if stdout_too_large || output.is_none() || stderr_bytes.is_none() {
            kill_plugin_group(&mut child);
        }
        let mut status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                kill_plugin_group(&mut child);
                let _ = out_reader.join();
                let _ = err_reader.join();
                return Err(anyhow!(
                    "plugin '{}': wait failed during {stage} stage: {error}",
                    self.name
                ));
            }
        };
        if status.is_none() {
            // The pipes closed but the plugin itself is still running.
            kill_plugin_group(&mut child);
            status = child.try_wait().ok().flatten();
        }

        if stdout_too_large {
            return Err(self.invocation_error(
                stage,
                format!("stdout exceeded the {MAX_STDOUT_BYTES} byte limit"),
                stderr_tail(stderr_bytes.unwrap_or_default()),
            ));
        }

        let Some(status) = status else {
            let stderr_text = stderr_tail(stderr_bytes.unwrap_or_default());
            return Err(self.invocation_error(
                stage,
                format!("timed out after {}ms", self.spec.timeout_ms),
                stderr_text,
            ));
        };

        let output = match output {
            Some(output) => output,
            None => {
                // The pipe never closed. If the group kill is what stopped the
                // plugin, it was still running at the deadline; otherwise the
                // plugin had already exited and a grandchild held the pipe.
                let detail = if terminated_by_group_kill(&status) {
                    format!("timed out after {}ms", self.spec.timeout_ms)
                } else {
                    "left processes running that hold its stdout open".to_string()
                };
                return Err(self.invocation_error(
                    stage,
                    detail,
                    stderr_tail(stderr_bytes.unwrap_or_default()),
                ));
            }
        };
        let stderr_text = stderr_tail(stderr_bytes.unwrap_or_default());

        if !status.success() {
            let detail = match status.code() {
                Some(code) => format!("exited with status {code}"),
                None => "terminated by signal".to_string(),
            };
            return Err(self.invocation_error(stage, detail, stderr_text));
        }

        if output.is_empty() {
            return Err(self.invocation_error(stage, "produced no stdout".into(), stderr_text));
        }

        serde_json::from_slice(&output).map_err(|error| {
            self.invocation_error(
                stage,
                format!("stdout is not valid JSON: {error}"),
                stderr_text,
            )
        })
    }

    fn invocation_error(&self, stage: &str, detail: String, stderr: String) -> anyhow::Error {
        if stderr.is_empty() {
            anyhow!("plugin '{}': {stage} stage {detail}", self.name)
        } else {
            anyhow!(
                "plugin '{}': {stage} stage {detail}; stderr: {stderr}",
                self.name
            )
        }
    }
}

/// Keep at most the last `STDERR_TAIL_BYTES` of captured stderr as UTF-8,
/// truncating on a char boundary so multibyte text never panics.
fn stderr_tail(bytes: Vec<u8>) -> String {
    if bytes.len() > STDERR_TAIL_BYTES {
        let start = bytes.len() - STDERR_TAIL_BYTES;
        let mut start = start;
        while start < bytes.len() && bytes[start] & 0xC0 == 0x80 {
            start += 1;
        }
        String::from_utf8_lossy(&bytes[start..]).into_owned()
    } else {
        String::from_utf8_lossy(&bytes).trim_end().to_string()
    }
}

impl Transformer for ExternalTransformer {
    fn name(&self) -> &str {
        &self.name
    }

    fn transform_request(&self, request: Value) -> Result<Value> {
        if self.spec.stages == PluginStages::Response {
            return Ok(request);
        }
        match self.run_stage("request", &request) {
            Ok(transformed) => Ok(transformed),
            Err(error) => match self.spec.on_error {
                PluginOnError::Passthrough => {
                    warn!(plugin = %self.name, error = %error, "plugin failed, passing request through unchanged");
                    Ok(request)
                }
                PluginOnError::Fail => Err(error),
            },
        }
    }

    fn transform_response(&self, response: Value) -> Result<Value> {
        if self.spec.stages == PluginStages::Request {
            return Ok(response);
        }
        match self.run_stage("response", &response) {
            Ok(transformed) => {
                debug!(plugin = %self.name, "plugin transformed response");
                Ok(transformed)
            }
            Err(error) => match self.spec.on_error {
                PluginOnError::Passthrough => {
                    warn!(plugin = %self.name, error = %error, "plugin failed, passing response through unchanged");
                    Ok(response)
                }
                PluginOnError::Fail => Err(error),
            },
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::PluginsConfig;
    use std::collections::HashMap;

    /// Write an executable `#!/bin/sh` script into a fresh temp dir and
    /// return a PluginSpec pointing at it.
    fn script_spec(
        dir: &std::path::Path,
        name: &str,
        body: &str,
    ) -> (Arc<PluginSpec>, std::path::PathBuf) {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let spec = PluginSpec {
            command: path.to_string_lossy().into_owned(),
            args: Vec::new(),
            timeout_ms: PluginSpec::DEFAULT_TIMEOUT_MS,
            env: HashMap::new(),
            stages: PluginStages::Both,
            on_error: PluginOnError::Passthrough,
        };
        (Arc::new(spec), path)
    }

    fn transformer(spec: Arc<PluginSpec>) -> ExternalTransformer {
        ExternalTransformer::new("test-plugin".to_string(), spec)
    }

    #[test]
    fn identity_script_round_trips_payload() {
        let dir = tempfile::tempdir().unwrap();
        let (spec, _) = script_spec(dir.path(), "identity.sh", "cat");
        let transformer = transformer(spec);
        let request = serde_json::json!({"model": "test", "messages": []});
        let result = transformer.transform_request(request.clone()).unwrap();
        assert_eq!(result, request);
        assert_eq!(transformer.name(), "test-plugin");
    }

    #[test]
    fn mutating_script_transforms_payload() {
        let dir = tempfile::tempdir().unwrap();
        // tr uppercases every byte; {"model":"aaa"} becomes {"MODEL":"AAA"},
        // which is still valid JSON and proves the payload flowed through.
        let (spec, _) = script_spec(dir.path(), "upper.sh", "tr 'a-z' 'A-Z'");
        let transformer = transformer(spec);
        let result = transformer
            .transform_request(serde_json::json!({"model": "aaa"}))
            .unwrap();
        assert_eq!(result, serde_json::json!({"MODEL": "AAA"}));
    }

    #[test]
    fn stage_and_name_env_reach_the_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let (spec, _) = script_spec(
            dir.path(),
            "envdump.sh",
            "printf '{\"stage\":\"%s\",\"plugin\":\"%s\"}' \"$CCR_STAGE\" \"$CCR_PLUGIN_NAME\"",
        );
        let transformer = transformer(spec);
        let response = transformer
            .transform_response(serde_json::json!({"unused": true}))
            .unwrap();
        assert_eq!(response["stage"], "response");
        assert_eq!(response["plugin"], "test-plugin");
    }

    #[test]
    fn spec_env_overrides_reach_the_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert("PLUGIN_TOKEN".to_string(), "secret-value".to_string());
        let (mut spec, _) = script_spec(
            dir.path(),
            "envvar.sh",
            "printf '{\"seen\":\"%s\"}' \"$PLUGIN_TOKEN\"",
        );
        Arc::get_mut(&mut spec).unwrap().env = env;
        let transformer = transformer(spec);
        let result = transformer
            .transform_request(serde_json::json!({"x": 1}))
            .unwrap();
        assert_eq!(result["seen"], "secret-value");
    }

    #[test]
    fn passthrough_default_on_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let (spec, _) = script_spec(dir.path(), "fail.sh", "echo boom >&2; exit 3");
        let transformer = transformer(spec);
        let request = serde_json::json!({"model": "test"});
        let result = transformer.transform_request(request.clone()).unwrap();
        assert_eq!(
            result, request,
            "passthrough must return the input unchanged"
        );
    }

    #[test]
    fn on_error_fail_propagates_with_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let (mut spec, _) = script_spec(dir.path(), "fail.sh", "echo boom >&2; exit 3");
        Arc::get_mut(&mut spec).unwrap().on_error = PluginOnError::Fail;
        let transformer = transformer(spec);
        let error = transformer
            .transform_request(serde_json::json!({"model": "test"}))
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("status 3"), "unexpected error: {text}");
        assert!(text.contains("boom"), "stderr tail missing: {text}");
    }

    #[test]
    fn on_error_fail_on_invalid_json_output() {
        let dir = tempfile::tempdir().unwrap();
        let (mut spec, _) = script_spec(dir.path(), "garbage.sh", "echo not-json");
        Arc::get_mut(&mut spec).unwrap().on_error = PluginOnError::Fail;
        let transformer = transformer(spec);
        let error = transformer
            .transform_response(serde_json::json!({"x": 1}))
            .unwrap_err();
        assert!(
            error.to_string().contains("not valid JSON"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn on_error_fail_on_empty_output() {
        let dir = tempfile::tempdir().unwrap();
        let (mut spec, _) = script_spec(dir.path(), "silent.sh", "exit 0");
        Arc::get_mut(&mut spec).unwrap().on_error = PluginOnError::Fail;
        let transformer = transformer(spec);
        let error = transformer
            .transform_request(serde_json::json!({"x": 1}))
            .unwrap_err();
        assert!(
            error.to_string().contains("no stdout"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn timeout_kills_plugin_and_passthrough_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("sleep.pid");
        let (mut spec, _) = script_spec(dir.path(), "slow.sh", &slow_script_body(&pid_file));
        Arc::get_mut(&mut spec).unwrap().timeout_ms = 100;
        let transformer = transformer(spec);
        let request = serde_json::json!({"model": "test"});
        let started = Instant::now();
        let result = transformer.transform_request(request.clone()).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(result, request);
        assert!(
            elapsed < Duration::from_secs(5),
            "timeout must not wait for the sleep, took {elapsed:?}"
        );
        let pid = recorded_pid(&pid_file);
        assert!(
            wait_for_exit(pid),
            "descendant {pid} outlived the timed-out plugin"
        );
    }

    #[test]
    fn timeout_with_on_error_fail_reports_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("sleep.pid");
        let (mut spec, _) = script_spec(dir.path(), "slow.sh", &slow_script_body(&pid_file));
        Arc::get_mut(&mut spec).unwrap().timeout_ms = 100;
        Arc::get_mut(&mut spec).unwrap().on_error = PluginOnError::Fail;
        let transformer = transformer(spec);
        let error = transformer
            .transform_request(serde_json::json!({"x": 1}))
            .unwrap_err();
        assert!(
            error.to_string().contains("timed out"),
            "unexpected error: {error}"
        );
        let pid = recorded_pid(&pid_file);
        assert!(
            wait_for_exit(pid),
            "descendant {pid} outlived the timed-out plugin"
        );
    }

    /// Wait for a pid to disappear. A zombie still answers `kill -0`, so this
    /// polls until the process is reaped by its new parent.
    fn wait_for_exit(pid: i32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    /// Body for a plugin that blocks on a `sleep` descendant and records its
    /// pid, so a test can assert the descendant dies with the plugin group.
    fn slow_script_body(pid_file: &std::path::Path) -> String {
        format!("sleep 30 & echo $! > '{}'; wait $!", pid_file.display())
    }

    /// Read the pid a test script recorded for its `sleep` descendant.
    fn recorded_pid(path: &std::path::Path) -> i32 {
        std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .parse()
            .expect("recorded descendant pid")
    }

    /// A plugin that exits while a backgrounded grandchild keeps its stdout
    /// open must not leak that grandchild, and the call must stay bounded by
    /// the plugin timeout instead of the grandchild's lifetime.
    #[test]
    fn backgrounded_grandchild_is_torn_down_with_the_plugin_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        let body = format!(
            "sleep 30 & echo $! > '{}'; echo '{{\"ok\":true}}'",
            pid_file.display()
        );
        let (mut spec, _) = script_spec(dir.path(), "leaky.sh", &body);
        Arc::get_mut(&mut spec).unwrap().timeout_ms = 300;
        let transformer = transformer(spec);
        let request = serde_json::json!({"model": "test"});
        let started = Instant::now();
        let result = transformer.transform_request(request.clone()).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(result, request);
        assert!(
            elapsed < Duration::from_secs(10),
            "call must be bounded by the plugin timeout, took {elapsed:?}"
        );
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .expect("grandchild pid");
        assert!(
            wait_for_exit(pid),
            "grandchild {pid} outlived the plugin process group"
        );
    }

    #[test]
    fn oversized_stdout_terminates_the_invocation() {
        let dir = tempfile::tempdir().unwrap();
        let (mut spec, _) = script_spec(
            dir.path(),
            "flood.sh",
            &format!(
                "yes '{{\"k\":\"v\"}}' | head -c {}",
                MAX_STDOUT_BYTES + 4096
            ),
        );
        Arc::get_mut(&mut spec).unwrap().timeout_ms = 2_000;
        Arc::get_mut(&mut spec).unwrap().on_error = PluginOnError::Fail;
        let transformer = transformer(spec);
        let error = transformer
            .transform_request(serde_json::json!({"x": 1}))
            .unwrap_err();
        assert!(
            error.to_string().contains("exceeded"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn response_only_plugin_skips_request_stage() {
        let dir = tempfile::tempdir().unwrap();
        // The plugin would corrupt the payload if it ran; a marker file
        // proves it never spawned for the request stage.
        let marker = dir.path().join("marker");
        let body = format!("touch {:?}", marker);
        let (mut spec, _) = script_spec(dir.path(), "resp-only.sh", &body);
        Arc::get_mut(&mut spec).unwrap().stages = PluginStages::Response;
        let transformer = transformer(spec);
        let request = serde_json::json!({"model": "test"});
        assert_eq!(
            transformer.transform_request(request.clone()).unwrap(),
            request
        );
        assert!(
            !marker.exists(),
            "request stage must not spawn a response-only plugin"
        );
        transformer
            .transform_response(serde_json::json!({"x": 1}))
            .unwrap();
        assert!(
            marker.exists(),
            "response stage must spawn a response-only plugin"
        );
    }

    #[test]
    fn request_only_plugin_skips_response_stage() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let body = format!("touch {:?}", marker);
        let (mut spec, _) = script_spec(dir.path(), "req-only.sh", &body);
        Arc::get_mut(&mut spec).unwrap().stages = PluginStages::Request;
        let transformer = transformer(spec);
        let response = serde_json::json!({"x": 1});
        assert_eq!(
            transformer.transform_response(response.clone()).unwrap(),
            response
        );
        assert!(
            !marker.exists(),
            "response stage must not spawn a request-only plugin"
        );
    }

    #[test]
    fn missing_command_passthrough_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Arc::new(PluginSpec {
            command: dir
                .path()
                .join("does-not-exist")
                .to_string_lossy()
                .into_owned(),
            args: Vec::new(),
            timeout_ms: PluginSpec::DEFAULT_TIMEOUT_MS,
            env: HashMap::new(),
            stages: PluginStages::Both,
            on_error: PluginOnError::Passthrough,
        });
        let transformer = transformer(spec);
        let request = serde_json::json!({"model": "test"});
        assert_eq!(
            transformer.transform_request(request.clone()).unwrap(),
            request
        );
    }

    #[test]
    fn registry_with_plugins_resolves_and_applies() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("shout.sh");
        std::fs::write(&script, "#!/bin/sh\ntr 'a-z' 'A-Z'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let plugins: PluginsConfig = serde_json::from_value(serde_json::json!({
            "dir": dir.path().to_string_lossy(),
            "plugins": {}
        }))
        .unwrap();
        let registry = super::super::TransformerRegistry::new_with_plugins(&plugins);
        let entries = vec![crate::config::TransformerEntry::Name("shout".to_string())];
        let chain = registry.build_chain(&entries);
        assert_eq!(chain.len(), 1, "drop-in plugin must resolve by file stem");
        let result = chain
            .apply_request(serde_json::json!({"model": "aaa"}))
            .unwrap();
        assert_eq!(result, serde_json::json!({"MODEL": "AAA"}));
    }
}
