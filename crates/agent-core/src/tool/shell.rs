use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use provider::ToolSpec;
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time,
};
use tracing::{debug, warn};

use super::{ToolError, ToolExecutionContext, ToolExecutor, ToolResult};

pub const MIN_TIMEOUT_MS: u64 = 1_000;
pub const MAX_TIMEOUT_MS: u64 = 120_000;

#[derive(Debug, Clone)]
pub struct ShellToolConfig {
    pub workspace_root: PathBuf,
    pub max_timeout: Duration,
}

impl Default for ShellToolConfig {
    fn default() -> Self {
        Self {
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            max_timeout: Duration::from_millis(MAX_TIMEOUT_MS),
        }
    }
}

pub struct ShellTool {
    config: ShellToolConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellInput {
    pub command: String,
    pub cwd: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct PreparedShellCommand {
    pub command: String,
    pub cwd: PathBuf,
    pub timeout: Duration,
}

#[derive(Debug)]
struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

impl ShellTool {
    pub fn new(config: ShellToolConfig) -> Self {
        Self { config }
    }

    fn input_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["command"],
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Non-interactive shell command to execute in the workspace."
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory. Must stay within the allowed workspace root."
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": MIN_TIMEOUT_MS,
                    "maximum": MAX_TIMEOUT_MS
                }
            }
        })
    }

    fn parse_input(input: serde_json::Value) -> Result<ShellInput, ToolError> {
        serde_json::from_value::<ShellInput>(input)
            .map_err(|err| ToolError::InvalidInput(err.to_string()))
    }

    pub fn prepare_command(
        &self,
        input: ShellInput,
        ctx: &ToolExecutionContext,
    ) -> Result<PreparedShellCommand, ToolError> {
        if input.command.trim().is_empty() {
            return Err(ToolError::InvalidInput(
                "shell command must not be empty".to_string(),
            ));
        }

        let cwd = match input.cwd.as_deref() {
            Some(cwd) => {
                let cwd = PathBuf::from(cwd);
                if cwd.is_absolute() {
                    cwd
                } else {
                    ctx.cwd.join(cwd)
                }
            }
            None => ctx.cwd.clone(),
        };
        let cwd = self.validate_cwd(&cwd)?;

        let timeout = match input.timeout_ms {
            Some(timeout_ms) => {
                if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
                    return Err(ToolError::InvalidInput(format!(
                        "timeout_ms must be between {MIN_TIMEOUT_MS} and {MAX_TIMEOUT_MS}"
                    )));
                }
                Duration::from_millis(timeout_ms)
            }
            None => ctx.timeout,
        };
        if timeout < Duration::from_millis(MIN_TIMEOUT_MS) || timeout > self.config.max_timeout {
            return Err(ToolError::InvalidInput(format!(
                "timeout must be between {MIN_TIMEOUT_MS}ms and {}ms",
                self.config.max_timeout.as_millis()
            )));
        }

        Ok(PreparedShellCommand {
            command: input.command,
            cwd,
            timeout,
        })
    }

    fn validate_cwd(&self, cwd: &Path) -> Result<PathBuf, ToolError> {
        let workspace_root = self.config.workspace_root.canonicalize().map_err(|err| {
            ToolError::Execution(format!("failed to canonicalize workspace root: {err}"))
        })?;
        let cwd = cwd
            .canonicalize()
            .map_err(|err| ToolError::Execution(format!("invalid cwd: {err}")))?;
        if !cwd.starts_with(&workspace_root) {
            return Err(ToolError::Denied(format!(
                "cwd must stay within workspace root: {cwd:?}"
            )));
        }

        Ok(cwd)
    }

    fn shell_path() -> OsString {
        std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin"))
    }

    fn toolchain_home(variable: &str, default_directory: &str) -> Option<OsString> {
        std::env::var_os(variable).or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(default_directory).into_os_string())
        })
    }

    #[cfg(unix)]
    fn terminate_process_group(pid: u32) -> Result<(), ToolError> {
        let pid = rustix::process::Pid::from_raw(pid as i32).ok_or_else(|| {
            ToolError::Execution("shell process did not provide a valid pid".to_string())
        })?;
        rustix::process::kill_process_group(pid, rustix::process::Signal::KILL).map_err(|err| {
            ToolError::Execution(format!("failed to terminate shell process group: {err}"))
        })
    }

    async fn capture_limited<R>(mut reader: R, max_bytes: usize) -> std::io::Result<CapturedOutput>
    where
        R: AsyncRead + Unpin,
    {
        let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
        let mut buffer = [0_u8; 8 * 1024];
        let mut truncated = false;

        loop {
            let count = reader.read(&mut buffer).await?;
            if count == 0 {
                break;
            }

            let remaining = max_bytes.saturating_sub(bytes.len());
            let kept = count.min(remaining);
            bytes.extend_from_slice(&buffer[..kept]);
            truncated |= kept < count;
        }

        Ok(CapturedOutput { bytes, truncated })
    }

    async fn run_shell(
        &self,
        input: ShellInput,
        ctx: ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let prepared = self.prepare_command(input, &ctx)?;
        let command = prepared.command.clone();
        let cwd = prepared.cwd.clone();
        let timeout = prepared.timeout;
        debug!(
            cwd = %cwd.display(),
            timeout_ms = timeout.as_millis(),
            "starting shell tool"
        );

        let mut process = Command::new("sh");
        // TODO: Add a native Windows command implementation; the built-in shell targets Unix-like systems.
        process
            .arg("-c")
            .arg(command)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .env("PATH", Self::shell_path())
            .env("HOME", &cwd)
            .env("LANG", "C")
            .env("TERM", "dumb")
            .kill_on_drop(true);
        #[cfg(unix)]
        process.process_group(0);
        if let Some(cargo_home) = Self::toolchain_home("CARGO_HOME", ".cargo") {
            process.env("CARGO_HOME", cargo_home);
        }
        if let Some(rustup_home) = Self::toolchain_home("RUSTUP_HOME", ".rustup") {
            process.env("RUSTUP_HOME", rustup_home);
        }

        let mut child = process
            .spawn()
            .map_err(|err| ToolError::Execution(format!("failed to spawn shell: {err}")))?;
        #[cfg(unix)]
        let process_group_id = child.id().ok_or_else(|| {
            ToolError::Execution("shell process did not provide a pid".to_string())
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ToolError::Execution("shell stdout was not captured".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ToolError::Execution("shell stderr was not captured".to_string()))?;

        let completed = time::timeout(timeout, async {
            tokio::try_join!(
                child.wait(),
                Self::capture_limited(stdout, ctx.max_stdout_bytes),
                Self::capture_limited(stderr, ctx.max_stderr_bytes),
            )
        })
        .await;

        let (status, stdout, stderr) = match completed {
            Ok(result) => result
                .map_err(|err| ToolError::Execution(format!("failed to wait for shell: {err}")))?,
            Err(_) => {
                warn!(timeout_ms = timeout.as_millis(), "shell tool timed out");
                #[cfg(unix)]
                {
                    Self::terminate_process_group(process_group_id)?;
                    let _ = child.wait().await;
                }
                #[cfg(not(unix))]
                let _ = child.kill().await;
                return Err(ToolError::Timeout(timeout));
            }
        };
        let stdout_truncated = stdout.truncated;
        let stderr_truncated = stderr.truncated;
        let stdout = String::from_utf8_lossy(&stdout.bytes).to_string();
        let stderr = String::from_utf8_lossy(&stderr.bytes).to_string();
        let exit_code = status.code();
        let success = status.success();
        debug!(
            success,
            exit_code = ?exit_code,
            stdout_truncated,
            stderr_truncated,
            "shell tool completed"
        );

        let content = format!("exit_code: {exit_code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}");
        let metadata = serde_json::json!({
            "exit_code": exit_code,
            "success": success,
            "stdout_truncated": stdout_truncated,
            "stderr_truncated": stderr_truncated,
            "cwd": cwd,
        });

        let result = if success {
            ToolResult::success(content)
        } else {
            ToolResult::failure(content)
        };

        Ok(result.with_metadata(metadata))
    }
}

#[async_trait::async_trait]
impl ToolExecutor for ShellTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "shell".to_string(),
            description: "Execute a non-interactive shell command in the current workspace."
                .to_string(),
            input_schema: Self::input_schema(),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        ctx: ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let input = Self::parse_input(input)?;
        self.run_shell(input, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    fn context(cwd: PathBuf) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd,
            timeout: Duration::from_secs(2),
            max_stdout_bytes: 64,
            max_stderr_bytes: 64,
        }
    }

    #[test]
    fn prepare_command_resolves_relative_cwd_under_context_directory() {
        let root = tempdir().unwrap();
        let nested = root.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let tool = ShellTool::new(ShellToolConfig {
            workspace_root: root.path().to_path_buf(),
            max_timeout: Duration::from_secs(30),
        });

        let prepared = tool
            .prepare_command(
                ShellInput {
                    command: "pwd".to_string(),
                    cwd: Some("nested".to_string()),
                    timeout_ms: Some(1_000),
                },
                &context(root.path().to_path_buf()),
            )
            .unwrap();

        assert_eq!(prepared.cwd, nested.canonicalize().unwrap());
    }

    #[test]
    fn prepare_command_rejects_cwd_outside_workspace() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let tool = ShellTool::new(ShellToolConfig {
            workspace_root: root.path().to_path_buf(),
            max_timeout: Duration::from_secs(30),
        });

        let err = tool
            .prepare_command(
                ShellInput {
                    command: "pwd".to_string(),
                    cwd: Some(outside.path().display().to_string()),
                    timeout_ms: Some(1_000),
                },
                &context(root.path().to_path_buf()),
            )
            .unwrap_err();

        assert!(matches!(err, ToolError::Denied(_)));
    }

    #[test]
    fn prepare_command_enforces_timeout_bounds() {
        let root = tempdir().unwrap();
        let tool = ShellTool::new(ShellToolConfig {
            workspace_root: root.path().to_path_buf(),
            max_timeout: Duration::from_secs(30),
        });

        let err = tool
            .prepare_command(
                ShellInput {
                    command: "pwd".to_string(),
                    cwd: None,
                    timeout_ms: Some(MAX_TIMEOUT_MS + 1),
                },
                &context(root.path().to_path_buf()),
            )
            .unwrap_err();

        assert!(matches!(err, ToolError::InvalidInput(_)));
    }

    #[test]
    fn prepare_command_accepts_model_supplied_timeout_up_to_the_default_limit() {
        let root = tempdir().unwrap();
        let tool = ShellTool::new(ShellToolConfig {
            workspace_root: root.path().to_path_buf(),
            max_timeout: Duration::from_millis(MAX_TIMEOUT_MS),
        });

        let prepared = tool
            .prepare_command(
                ShellInput {
                    command: "pwd".to_string(),
                    cwd: None,
                    timeout_ms: Some(MAX_TIMEOUT_MS),
                },
                &context(root.path().to_path_buf()),
            )
            .unwrap();

        assert_eq!(prepared.timeout, Duration::from_millis(MAX_TIMEOUT_MS));
    }

    #[tokio::test]
    async fn shell_captures_output_without_inheriting_api_key() {
        let root = tempdir().unwrap();
        let tool = ShellTool::new(ShellToolConfig {
            workspace_root: root.path().to_path_buf(),
            max_timeout: Duration::from_secs(30),
        });

        let result = tool
            .execute(
                json!({"command": "test -z \"${OPENAI_API_KEY:-}\""}),
                context(root.path().to_path_buf()),
            )
            .await
            .unwrap();

        assert!(result.success);
    }

    #[tokio::test]
    async fn shell_preserves_rust_toolchain_discovery_without_inheriting_home() {
        let root = tempdir().unwrap();
        let tool = ShellTool::new(ShellToolConfig {
            workspace_root: root.path().to_path_buf(),
            max_timeout: Duration::from_secs(30),
        });

        let result = tool
            .execute(
                json!({"command": "cargo --version"}),
                context(root.path().to_path_buf()),
            )
            .await
            .unwrap();

        assert!(result.success, "{}", result.content);
    }

    #[tokio::test]
    async fn shell_bounds_captured_output_and_kills_timed_out_process_group() {
        let root = tempdir().unwrap();
        let tool = ShellTool::new(ShellToolConfig {
            workspace_root: root.path().to_path_buf(),
            max_timeout: Duration::from_secs(30),
        });
        let limited_context = ToolExecutionContext {
            max_stdout_bytes: 8,
            ..context(root.path().to_path_buf())
        };

        let result = tool
            .execute(json!({"command": "yes x | head -c 128"}), limited_context)
            .await
            .unwrap();
        assert_eq!(result.metadata["stdout_truncated"], true);

        let marker = root.path().join("background-command-completed");
        let err = tool
            .execute(
                json!({
                    "command": format!("(sleep 2; touch {}) &", marker.display()),
                    "timeout_ms": 1000,
                }),
                context(root.path().to_path_buf()),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Timeout(_)));
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert!(!marker.exists());
    }
}
