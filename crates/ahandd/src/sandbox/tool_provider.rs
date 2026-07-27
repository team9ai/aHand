use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;

use crate::app_tool_registry::{AppToolDef, AppToolError, AppToolHandler, AppToolInvocation};
use crate::sandbox::{
    file_lifecycle,
    registry::SandboxRegistry,
    types::{
        HostFileRef, RuntimeExecuteResult, SandboxCommand, SandboxError, SandboxExecRequest,
        SandboxExecResult, SandboxFile, SandboxInvocationContext, SandboxResult,
    },
};

pub const CODE_SANDBOX_CONTEXT_REQUIRED: &str = "SANDBOX_CONTEXT_REQUIRED";

#[derive(Debug, Clone, Copy)]
pub struct SandboxToolProviderOptions {
    pub include_compat_aliases: bool,
}

pub trait SandboxInvocationPermit: Send {}

impl<T: Send> SandboxInvocationPermit for T {}

pub trait SandboxInvocationResolver: Send + Sync {
    fn resolve(&self, invocation: &AppToolInvocation) -> SandboxResult<SandboxInvocationContext>;

    fn resolve_host_file_ref(
        &self,
        context: &SandboxInvocationContext,
        file_ref_id: &str,
    ) -> SandboxResult<HostFileRef> {
        let _ = context;
        Err(SandboxError::unknown_file_ref(format!(
            "unknown host file reference '{file_ref_id}'"
        )))
    }

    fn ensure_session(
        &self,
        registry: &mut SandboxRegistry,
        context: &SandboxInvocationContext,
    ) -> SandboxResult<()> {
        let _ = (registry, context);
        Ok(())
    }

    fn acquire_session(
        &self,
        registry: &mut SandboxRegistry,
        context: &SandboxInvocationContext,
    ) -> SandboxResult<Box<dyn SandboxInvocationPermit>> {
        self.ensure_session(registry, context)?;
        Ok(Box::new(()))
    }
}

#[derive(Debug, Clone)]
pub struct FixedSandboxInvocationResolver {
    session_id: String,
}

impl FixedSandboxInvocationResolver {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
        }
    }
}

impl SandboxInvocationResolver for FixedSandboxInvocationResolver {
    fn resolve(&self, invocation: &AppToolInvocation) -> SandboxResult<SandboxInvocationContext> {
        let context = invocation.context.as_ref().ok_or_else(|| {
            SandboxError::new(
                CODE_SANDBOX_CONTEXT_REQUIRED,
                "sandbox tool invocation requires trusted context",
            )
        })?;

        let session_id = context
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|session_id| !session_id.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| self.session_id.clone());

        Ok(SandboxInvocationContext {
            session_id,
            run_id: trusted_string(context, "runId"),
            scope_id: trusted_string(context, "scopeId"),
            invocation_id: trusted_string(context, "invocationId"),
        })
    }
}

#[cfg(test)]
#[derive(Default)]
struct ExecutionCaptureGate {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[derive(Clone)]
pub struct SandboxToolProvider {
    registry: Arc<AsyncMutex<SandboxRegistry>>,
    resolver: Arc<dyn SandboxInvocationResolver>,
    options: SandboxToolProviderOptions,
    sandbox_state_root: PathBuf,
    #[cfg(test)]
    captured_exec: Option<Arc<AsyncMutex<Option<SandboxExecRequest>>>>,
    #[cfg(test)]
    captured_exec_gate: Option<Arc<ExecutionCaptureGate>>,
}

impl SandboxToolProvider {
    pub fn new(
        registry: Arc<AsyncMutex<SandboxRegistry>>,
        resolver: Arc<dyn SandboxInvocationResolver>,
        options: SandboxToolProviderOptions,
    ) -> Self {
        Self::new_with_sandbox_state_root(registry, resolver, options, default_sandbox_state_root())
    }

    pub fn new_with_sandbox_state_root(
        registry: Arc<AsyncMutex<SandboxRegistry>>,
        resolver: Arc<dyn SandboxInvocationResolver>,
        options: SandboxToolProviderOptions,
        sandbox_state_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            registry,
            resolver,
            options,
            sandbox_state_root: sandbox_state_root.into(),
            #[cfg(test)]
            captured_exec: None,
            #[cfg(test)]
            captured_exec_gate: None,
        }
    }

    #[cfg(test)]
    pub fn new_for_test_with_exec_capture(
        registry: Arc<AsyncMutex<SandboxRegistry>>,
        resolver: Arc<dyn SandboxInvocationResolver>,
        options: SandboxToolProviderOptions,
        captured_exec: Arc<AsyncMutex<Option<SandboxExecRequest>>>,
    ) -> Self {
        Self {
            registry,
            resolver,
            options,
            sandbox_state_root: default_sandbox_state_root(),
            captured_exec: Some(captured_exec),
            captured_exec_gate: None,
        }
    }

    pub fn tool_handlers(&self) -> Vec<(AppToolDef, AppToolHandler)> {
        let mut tools = vec![
            (import_file_def(), self.import_file_handler()),
            (
                run_command_def("run_command"),
                self.run_command_handler("run_command"),
            ),
        ];

        if self.options.include_compat_aliases {
            tools.push((
                run_command_def("sandbox_exec"),
                self.run_command_handler("sandbox_exec"),
            ));
            tools.push((run_node_def(), self.run_node_handler()));
        }

        tools
    }

    fn import_file_handler(&self) -> AppToolHandler {
        let provider = self.clone();
        Arc::new(move |invocation| {
            let provider = provider.clone();
            let future: BoxFuture<'static, Result<Value, AppToolError>> =
                Box::pin(async move { provider.import_file(invocation).await });
            future
        })
    }

    fn run_command_handler(&self, _tool_name: &'static str) -> AppToolHandler {
        let provider = self.clone();
        Arc::new(move |invocation| {
            let provider = provider.clone();
            let future: BoxFuture<'static, Result<Value, AppToolError>> =
                Box::pin(async move { provider.run_command(invocation).await });
            future
        })
    }

    fn run_node_handler(&self) -> AppToolHandler {
        let provider = self.clone();
        Arc::new(move |invocation| {
            let provider = provider.clone();
            let future: BoxFuture<'static, Result<Value, AppToolError>> =
                Box::pin(async move { provider.run_node(invocation).await });
            future
        })
    }

    async fn acquire_context_session(
        &self,
        context: &SandboxInvocationContext,
    ) -> Result<Box<dyn SandboxInvocationPermit>, AppToolError> {
        let mut registry = self.registry.lock().await;
        self.resolver
            .acquire_session(&mut registry, context)
            .map_err(app_tool_error_from_sandbox)
    }

    async fn import_file(&self, invocation: AppToolInvocation) -> Result<Value, AppToolError> {
        let context = self
            .resolver
            .resolve(&invocation)
            .map_err(app_tool_error_from_sandbox)?;
        let file_ref_id = require_string_arg(&invocation.args, "fileRefId")?;
        let mut host_file_ref = self
            .resolver
            .resolve_host_file_ref(&context, &file_ref_id)
            .map_err(app_tool_error_from_sandbox)?;
        if host_file_ref.conversation_id.is_none() {
            host_file_ref.conversation_id = context.run_id.clone();
        }
        let permit = self.acquire_context_session(&context).await?;

        let file = {
            let mut registry = self.registry.lock().await;
            file_lifecycle::import_file(&mut registry, &context.session_id, host_file_ref)
        }
        .map_err(app_tool_error_from_sandbox)?;
        drop(permit);

        Ok(sandbox_file_json(&file))
    }

    async fn run_command(&self, invocation: AppToolInvocation) -> Result<Value, AppToolError> {
        let context = self
            .resolver
            .resolve(&invocation)
            .map_err(app_tool_error_from_sandbox)?;
        let command = parse_sandbox_command_arg(&invocation.args)?;
        let cwd = optional_string_arg(&invocation.args, "cwd")?.map(PathBuf::from);
        let env = optional_string_map_arg(&invocation.args, "env")?;
        let timeout = optional_timeout_arg(&invocation.args, "timeoutSeconds")?;
        let permit = self.acquire_context_session(&context).await?;
        let session_id = context.session_id.clone();
        let result = self
            .execute_command(
                &session_id,
                SandboxExecRequest {
                    command,
                    cwd,
                    env,
                    timeout,
                    context: Some(context),
                },
            )
            .await
            .map_err(app_tool_error_from_sandbox)?;
        drop(permit);

        Ok(runtime_execute_result_json(&result))
    }

    async fn run_node(&self, invocation: AppToolInvocation) -> Result<Value, AppToolError> {
        let context = self
            .resolver
            .resolve(&invocation)
            .map_err(app_tool_error_from_sandbox)?;
        let args = require_string_array_arg(&invocation.args, "args")?;
        let cwd = optional_string_arg(&invocation.args, "cwd")?.map(PathBuf::from);
        let timeout = optional_timeout_arg(&invocation.args, "timeoutSeconds")?;
        let command = SandboxCommand::Argv {
            command: std::iter::once("node".to_string()).chain(args).collect(),
        };
        let permit = self.acquire_context_session(&context).await?;
        let session_id = context.session_id.clone();
        let result = self
            .execute_command(
                &session_id,
                SandboxExecRequest {
                    command,
                    cwd,
                    env: HashMap::new(),
                    timeout,
                    context: Some(context),
                },
            )
            .await
            .map_err(app_tool_error_from_sandbox)?;
        drop(permit);

        Ok(runtime_execute_result_json(&result))
    }

    async fn execute_command(
        &self,
        session_id: &str,
        request: SandboxExecRequest,
    ) -> SandboxResult<SandboxExecResult> {
        #[cfg(test)]
        if let Some(captured_exec) = &self.captured_exec {
            *captured_exec.lock().await = Some(request);
            if let Some(gate) = &self.captured_exec_gate {
                gate.started.notify_one();
                gate.release.notified().await;
            }
            return Ok(RuntimeExecuteResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: Some(0),
                timed_out: false,
            });
        }

        crate::public_api::execute_sandbox_command_with_registry(
            Arc::clone(&self.registry),
            self.sandbox_state_root.clone(),
            session_id,
            request,
        )
        .await
    }
}

fn default_sandbox_state_root() -> PathBuf {
    std::env::temp_dir().join("ahand-windows-sandbox")
}

pub fn invalid_arg(argument: &str, message: impl Into<String>) -> AppToolError {
    AppToolError {
        code: "INVALID_ARGUMENT".to_string(),
        message: format!(
            "invalid sandbox tool argument '{argument}': {}",
            message.into()
        ),
    }
}

pub fn require_string_arg(args: &Value, name: &str) -> Result<String, AppToolError> {
    optional_string_arg(args, name)?
        .ok_or_else(|| invalid_arg(name, "is required and must be a string"))
}

pub fn optional_string_arg(args: &Value, name: &str) -> Result<Option<String>, AppToolError> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid_arg(name, "must be a string")),
    }
}

pub fn require_string_array_arg(args: &Value, name: &str) -> Result<Vec<String>, AppToolError> {
    let Some(value) = args.get(name) else {
        return Err(invalid_arg(
            name,
            "is required and must be an array of strings",
        ));
    };
    let Some(items) = value.as_array() else {
        return Err(invalid_arg(name, "must be an array of strings"));
    };

    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid_arg(name, "must be an array of strings"))
        })
        .collect()
}

pub fn require_non_empty_string_array_arg(
    args: &Value,
    name: &str,
) -> Result<Vec<String>, AppToolError> {
    let items = require_string_array_arg(args, name)?;
    if items.is_empty() {
        return Err(invalid_arg(name, "must contain at least one item"));
    }
    Ok(items)
}

fn parse_sandbox_command_arg(args: &Value) -> Result<SandboxCommand, AppToolError> {
    let has_cmd = args.get("cmd").is_some();
    let has_command = args.get("command").is_some();

    match (has_cmd, has_command) {
        (true, false) => {
            let cmd = require_string_arg(args, "cmd")?;
            if cmd.trim().is_empty() {
                return Err(invalid_arg("cmd", "must not be empty"));
            }
            Ok(SandboxCommand::Shell { cmd })
        }
        (false, true) => Ok(SandboxCommand::Argv {
            command: require_non_empty_string_array_arg(args, "command")?,
        }),
        (true, true) => Err(invalid_arg("cmd", "provide exactly one of cmd or command")),
        (false, false) => Err(invalid_arg("cmd", "provide exactly one of cmd or command")),
    }
}

pub fn optional_string_map_arg(
    args: &Value,
    name: &str,
) -> Result<HashMap<String, String>, AppToolError> {
    let Some(value) = args.get(name) else {
        return Ok(HashMap::new());
    };
    if value.is_null() {
        return Ok(HashMap::new());
    }

    let Some(object) = value.as_object() else {
        return Err(invalid_arg(name, "must be an object with string values"));
    };

    object
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
                .ok_or_else(|| invalid_arg(name, "must be an object with string values"))
        })
        .collect()
}

pub fn optional_timeout_arg(args: &Value, name: &str) -> Result<Option<Duration>, AppToolError> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    let Some(seconds) = value.as_u64() else {
        return Err(invalid_arg(name, "must be an integer from 1 to 600"));
    };
    if !(1..=600).contains(&seconds) {
        return Err(invalid_arg(name, "must be an integer from 1 to 600"));
    }

    Ok(Some(Duration::from_secs(seconds)))
}

pub fn app_tool_error_from_sandbox(error: SandboxError) -> AppToolError {
    AppToolError {
        code: error.code,
        message: error.message,
    }
}

fn runtime_execute_result_json(result: &RuntimeExecuteResult) -> Value {
    json!({
        "stdout": result.stdout,
        "stderr": result.stderr,
        "exitCode": result.exit_code,
        "timedOut": result.timed_out,
    })
}

fn sandbox_file_json(file: &SandboxFile) -> Value {
    json!({
        "sandboxFileId": file.sandbox_file_id,
        "fileRefId": file.file_ref_id,
        "sandboxPath": file.sandbox_path.to_string_lossy().to_string(),
        "size": file.size,
    })
}

fn trusted_string(context: &Value, name: &str) -> Option<String> {
    context
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn import_file_def() -> AppToolDef {
    AppToolDef {
        name: "import_file".to_string(),
        description: "Import a trusted host file reference into the sandbox".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "fileRefId": { "type": "string" }
            },
            "required": ["fileRefId"],
            "additionalProperties": false
        }),
        requires_approval: false,
    }
}

fn run_command_def(name: &'static str) -> AppToolDef {
    AppToolDef {
        name: name.to_string(),
        description: "Run a command inside the sandbox".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "cmd": {
                    "type": "string",
                    "minLength": 1
                },
                "command": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1
                },
                "cwd": { "type": "string" },
                "env": {
                    "type": "object",
                    "additionalProperties": { "type": "string" }
                },
                "timeoutSeconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 600
                }
            },
            "oneOf": [
                { "required": ["cmd"], "not": { "required": ["command"] } },
                { "required": ["command"], "not": { "required": ["cmd"] } }
            ],
            "additionalProperties": false
        }),
        requires_approval: false,
    }
}

fn run_node_def() -> AppToolDef {
    AppToolDef {
        name: "run_node".to_string(),
        description: "Run Node.js inside the sandbox".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "args": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "cwd": { "type": "string" },
                "timeoutSeconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 600
                }
            },
            "required": ["args"],
            "additionalProperties": false
        }),
        requires_approval: false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::sync::Mutex as AsyncMutex;

    use super::*;
    use crate::app_tool_registry::AppToolInvocation;
    use crate::sandbox::registry::SandboxRegistry;
    use crate::sandbox::types::{
        HostFileRef, NetworkPolicy, SandboxCommand, SandboxExecRequest, SandboxPermissionMode,
        SandboxSessionConfig,
    };

    fn provider(include_compat_aliases: bool) -> SandboxToolProvider {
        SandboxToolProvider::new(
            Arc::new(AsyncMutex::new(SandboxRegistry::default())),
            Arc::new(FixedSandboxInvocationResolver::new("fixed-session")),
            SandboxToolProviderOptions {
                include_compat_aliases,
            },
        )
    }

    fn tool_names(provider: &SandboxToolProvider) -> BTreeSet<String> {
        provider
            .tool_handlers()
            .into_iter()
            .map(|(def, _handler)| def.name)
            .collect()
    }

    fn handler(provider: &SandboxToolProvider, name: &str) -> AppToolHandler {
        provider
            .tool_handlers()
            .into_iter()
            .find(|(def, _)| def.name == name)
            .map(|(_, handler)| handler)
            .expect("tool handler registered")
    }

    fn invocation(args: Value, context: Option<Value>) -> AppToolInvocation {
        AppToolInvocation {
            tool_call_id: "call-1".to_string(),
            name: "run_command".to_string(),
            args,
            timeout_ms: 5_000,
            context,
        }
    }

    fn trusted_context() -> Value {
        json!({
            "sessionId": "session-1",
            "runId": "run-1",
            "scopeId": "scope-1",
        })
    }

    fn registry_with_session(
        workspace_root: PathBuf,
        permission_mode: SandboxPermissionMode,
    ) -> Arc<AsyncMutex<SandboxRegistry>> {
        std::fs::create_dir_all(&workspace_root).unwrap();
        let mut registry = SandboxRegistry::default();
        registry
            .create_session(SandboxSessionConfig {
                session_id: "session-1".to_string(),
                permission_mode,
                workspace_root,
                network: NetworkPolicy::Enabled,
                mounts: Vec::new(),
            })
            .unwrap();
        Arc::new(AsyncMutex::new(registry))
    }

    #[derive(Debug)]
    struct HostFileResolver {
        source_path: PathBuf,
        file_ref_id: Option<String>,
    }

    impl SandboxInvocationResolver for HostFileResolver {
        fn resolve(
            &self,
            invocation: &AppToolInvocation,
        ) -> SandboxResult<SandboxInvocationContext> {
            FixedSandboxInvocationResolver::new("session-1").resolve(invocation)
        }

        fn resolve_host_file_ref(
            &self,
            context: &SandboxInvocationContext,
            file_ref_id: &str,
        ) -> SandboxResult<HostFileRef> {
            let _ = context;
            Ok(HostFileRef {
                file_ref_id: self
                    .file_ref_id
                    .clone()
                    .unwrap_or_else(|| file_ref_id.to_string()),
                source_path: self.source_path.clone(),
                display_name: "source.txt".to_string(),
                size: 5,
                mtime_ms: None,
                conversation_id: None,
            })
        }
    }

    #[derive(Debug)]
    struct EnsuringHostFileResolver {
        source_path: PathBuf,
        workspace_root: PathBuf,
    }

    impl SandboxInvocationResolver for EnsuringHostFileResolver {
        fn resolve(
            &self,
            invocation: &AppToolInvocation,
        ) -> SandboxResult<SandboxInvocationContext> {
            FixedSandboxInvocationResolver::new("device-session").resolve(invocation)
        }

        fn ensure_session(
            &self,
            registry: &mut SandboxRegistry,
            context: &SandboxInvocationContext,
        ) -> SandboxResult<()> {
            if registry.session(&context.session_id).is_ok() {
                return Ok(());
            }
            std::fs::create_dir_all(&self.workspace_root).unwrap();
            registry.create_session(SandboxSessionConfig {
                session_id: context.session_id.clone(),
                permission_mode: SandboxPermissionMode::Readonly,
                workspace_root: self.workspace_root.clone(),
                network: NetworkPolicy::Enabled,
                mounts: Vec::new(),
            })
        }

        fn resolve_host_file_ref(
            &self,
            _context: &SandboxInvocationContext,
            file_ref_id: &str,
        ) -> SandboxResult<HostFileRef> {
            Ok(HostFileRef {
                file_ref_id: file_ref_id.to_string(),
                source_path: self.source_path.clone(),
                display_name: "source.txt".to_string(),
                size: 5,
                mtime_ms: None,
                conversation_id: None,
            })
        }
    }

    #[derive(Debug)]
    struct TrackingPermit {
        active: Arc<AtomicUsize>,
    }

    impl Drop for TrackingPermit {
        fn drop(&mut self) {
            self.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[derive(Debug)]
    struct PermitTrackingResolver {
        active: Arc<AtomicUsize>,
    }

    impl SandboxInvocationResolver for PermitTrackingResolver {
        fn resolve(
            &self,
            _invocation: &AppToolInvocation,
        ) -> SandboxResult<SandboxInvocationContext> {
            Ok(SandboxInvocationContext {
                session_id: "session-1".to_string(),
                run_id: Some("run-1".to_string()),
                scope_id: None,
                invocation_id: None,
            })
        }

        fn acquire_session(
            &self,
            registry: &mut SandboxRegistry,
            context: &SandboxInvocationContext,
        ) -> SandboxResult<Box<dyn SandboxInvocationPermit>> {
            registry.session(&context.session_id)?;
            self.active.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(TrackingPermit {
                active: Arc::clone(&self.active),
            }))
        }
    }

    #[derive(Debug)]
    struct LegacyEnsuringResolver {
        ensure_calls: Arc<AtomicUsize>,
    }

    impl SandboxInvocationResolver for LegacyEnsuringResolver {
        fn resolve(
            &self,
            _invocation: &AppToolInvocation,
        ) -> SandboxResult<SandboxInvocationContext> {
            Ok(SandboxInvocationContext {
                session_id: "session-1".to_string(),
                run_id: Some("run-1".to_string()),
                scope_id: None,
                invocation_id: None,
            })
        }

        fn ensure_session(
            &self,
            registry: &mut SandboxRegistry,
            context: &SandboxInvocationContext,
        ) -> SandboxResult<()> {
            registry.session(&context.session_id)?;
            self.ensure_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn provider_retains_invocation_permit_through_command_execution() {
        let temp = tempfile::tempdir().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let captured_exec = Arc::new(AsyncMutex::new(None));
        let captured_exec_gate = Arc::new(ExecutionCaptureGate::default());
        let provider = SandboxToolProvider {
            registry: registry_with_session(
                temp.path().join("sandbox"),
                SandboxPermissionMode::Readonly,
            ),
            resolver: Arc::new(PermitTrackingResolver {
                active: Arc::clone(&active),
            }),
            options: SandboxToolProviderOptions {
                include_compat_aliases: true,
            },
            sandbox_state_root: default_sandbox_state_root(),
            captured_exec: Some(captured_exec),
            captured_exec_gate: Some(Arc::clone(&captured_exec_gate)),
        };

        let run_command = handler(&provider, "run_command");
        let invocation_task = tokio::spawn(async move {
            run_command(invocation(
                json!({"command": ["python", "-V"]}),
                Some(trusted_context()),
            ))
            .await
        });

        captured_exec_gate.started.notified().await;
        assert_eq!(active.load(Ordering::SeqCst), 1);
        captured_exec_gate.release.notify_one();

        let result = invocation_task.await.unwrap().unwrap();
        assert_eq!(result["exitCode"], json!(0));
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn legacy_resolver_uses_default_noop_invocation_permit() {
        let temp = tempfile::tempdir().unwrap();
        let ensure_calls = Arc::new(AtomicUsize::new(0));
        let captured_exec = Arc::new(AsyncMutex::new(None));
        let provider = SandboxToolProvider::new_for_test_with_exec_capture(
            registry_with_session(temp.path().join("sandbox"), SandboxPermissionMode::Readonly),
            Arc::new(LegacyEnsuringResolver {
                ensure_calls: Arc::clone(&ensure_calls),
            }),
            SandboxToolProviderOptions {
                include_compat_aliases: true,
            },
            captured_exec,
        );

        let result = handler(&provider, "run_command")(invocation(
            json!({"command": ["python", "-V"]}),
            Some(trusted_context()),
        ))
        .await
        .unwrap();

        assert_eq!(result["exitCode"], json!(0));
        assert_eq!(ensure_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn provider_registers_command_tools_without_version_lifecycle_tools() {
        let names = tool_names(&provider(true));

        assert!(names.contains("run_command"));
        assert!(names.contains("sandbox_exec"));
        assert!(names.contains("run_node"));
        assert!(names.contains("import_file"));
        assert!(!names.contains("register_file_version"));
        assert!(!names.contains("commit_file_version"));
    }

    #[test]
    fn provider_can_disable_compat_aliases() {
        let names = tool_names(&provider(false));

        assert!(names.contains("run_command"));
        assert!(names.contains("import_file"));
        assert!(!names.contains("sandbox_exec"));
        assert!(!names.contains("run_node"));
        assert!(!names.contains("register_file_version"));
        assert!(!names.contains("commit_file_version"));
    }

    #[test]
    fn import_file_schema_only_exposes_file_ref_id() {
        let schema = import_file_def().input_schema;
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .expect("required must be an array");
        let required_names = required.iter().map(Value::as_str).collect::<Vec<_>>();
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .expect("properties must be an object");

        assert_eq!(required_names, vec![Some("fileRefId")]);
        assert!(properties.contains_key("fileRefId"));
        assert!(!properties.contains_key("sourcePath"));
        assert!(!properties.contains_key("displayName"));
    }

    #[test]
    fn fixed_resolver_rejects_missing_context_and_ignores_spoofed_args_context() {
        let resolver = FixedSandboxInvocationResolver::new("fixed-session");
        let missing = resolver
            .resolve(&invocation(
                json!({"context": {"sessionId": "spoofed-session"}}),
                None,
            ))
            .unwrap_err();

        assert_eq!(missing.code, CODE_SANDBOX_CONTEXT_REQUIRED);
        assert_eq!(
            missing.message,
            "sandbox tool invocation requires trusted context"
        );

        let resolved = resolver
            .resolve(&invocation(
                json!({
                    "context": {
                        "sessionId": "spoofed-session",
                        "runId": "spoofed-run",
                        "scopeId": "spoofed-scope",
                    }
                }),
                Some(json!({
                    "sessionId": "trusted-session",
                    "runId": "trusted-run",
                    "scopeId": "trusted-scope",
                })),
            ))
            .unwrap();

        assert_eq!(resolved.session_id, "trusted-session");
        assert_eq!(resolved.run_id.as_deref(), Some("trusted-run"));
        assert_eq!(resolved.scope_id.as_deref(), Some("trusted-scope"));

        let fallback = resolver
            .resolve(&invocation(
                json!({"context": {"sessionId": "spoofed-session"}}),
                Some(json!({"sessionId": ""})),
            ))
            .unwrap();

        assert_eq!(fallback.session_id, "fixed-session");
    }

    #[tokio::test]
    async fn run_command_requires_trusted_context() {
        let provider = provider(true);
        let err =
            handler(&provider, "run_command")(invocation(json!({"command": ["echo", "ok"]}), None))
                .await
                .unwrap_err();

        assert_eq!(err.code, CODE_SANDBOX_CONTEXT_REQUIRED);
    }

    #[test]
    fn sandbox_command_timeout_supports_ten_minutes() {
        let args = json!({"timeoutSeconds": 600});
        assert_eq!(
            optional_timeout_arg(&args, "timeoutSeconds").unwrap(),
            Some(Duration::from_secs(600))
        );

        let error =
            optional_timeout_arg(&json!({"timeoutSeconds": 601}), "timeoutSeconds").unwrap_err();
        assert_eq!(error.code, "INVALID_ARGUMENT");
        assert!(error.message.contains("1 to 600"));

        assert_eq!(
            run_command_def("run_command").input_schema["properties"]["timeoutSeconds"]["maximum"],
            json!(600)
        );
        assert_eq!(
            run_node_def().input_schema["properties"]["timeoutSeconds"]["maximum"],
            json!(600)
        );
    }

    #[tokio::test]
    async fn run_node_wrapper_builds_node_command_request() {
        let captured_exec = Arc::new(AsyncMutex::new(None));
        let provider = SandboxToolProvider::new_for_test_with_exec_capture(
            Arc::new(AsyncMutex::new(SandboxRegistry::default())),
            Arc::new(FixedSandboxInvocationResolver::new("session-1")),
            SandboxToolProviderOptions {
                include_compat_aliases: true,
            },
            Arc::clone(&captured_exec),
        );

        let result = handler(&provider, "run_node")(invocation(
            json!({
                "args": ["script.js"],
                "cwd": "workspace",
                "timeoutSeconds": 7
            }),
            Some(trusted_context()),
        ))
        .await
        .unwrap();

        assert_eq!(result["exitCode"], json!(0));
        let captured: SandboxExecRequest = captured_exec.lock().await.clone().unwrap();
        assert_eq!(
            captured.command,
            SandboxCommand::Argv {
                command: vec!["node".to_string(), "script.js".to_string()]
            }
        );
        assert_eq!(captured.cwd, Some(PathBuf::from("workspace")));
        assert_eq!(captured.timeout, Some(Duration::from_secs(7)));
    }

    #[tokio::test]
    async fn run_command_accepts_shell_cmd_request() {
        let captured_exec = Arc::new(AsyncMutex::new(None));
        let provider = SandboxToolProvider::new_for_test_with_exec_capture(
            Arc::new(AsyncMutex::new(SandboxRegistry::default())),
            Arc::new(FixedSandboxInvocationResolver::new("session-1")),
            SandboxToolProviderOptions {
                include_compat_aliases: true,
            },
            Arc::clone(&captured_exec),
        );

        let result = handler(&provider, "run_command")(invocation(
            json!({
                "cmd": "echo ok",
                "cwd": "workspace",
                "env": { "EXAMPLE": "1" },
                "timeoutSeconds": 7
            }),
            Some(trusted_context()),
        ))
        .await
        .unwrap();

        assert_eq!(result["exitCode"], json!(0));
        let captured: SandboxExecRequest = captured_exec.lock().await.clone().unwrap();
        assert_eq!(
            captured.command,
            SandboxCommand::Shell {
                cmd: "echo ok".to_string()
            }
        );
        assert_eq!(captured.cwd, Some(PathBuf::from("workspace")));
        assert_eq!(captured.env["EXAMPLE"], "1");
        assert_eq!(captured.timeout, Some(Duration::from_secs(7)));
    }

    #[tokio::test]
    async fn run_command_accepts_legacy_argv_request() {
        let captured_exec = Arc::new(AsyncMutex::new(None));
        let provider = SandboxToolProvider::new_for_test_with_exec_capture(
            Arc::new(AsyncMutex::new(SandboxRegistry::default())),
            Arc::new(FixedSandboxInvocationResolver::new("session-1")),
            SandboxToolProviderOptions {
                include_compat_aliases: true,
            },
            Arc::clone(&captured_exec),
        );

        handler(&provider, "sandbox_exec")(invocation(
            json!({"command": ["python", "-c", "print('ok')"]}),
            Some(trusted_context()),
        ))
        .await
        .unwrap();

        let captured: SandboxExecRequest = captured_exec.lock().await.clone().unwrap();
        assert_eq!(
            captured.command,
            SandboxCommand::Argv {
                command: vec![
                    "python".to_string(),
                    "-c".to_string(),
                    "print('ok')".to_string(),
                ]
            }
        );
    }

    #[tokio::test]
    async fn run_command_rejects_cmd_and_command_together() {
        let captured_exec = Arc::new(AsyncMutex::new(None));
        let provider = SandboxToolProvider::new_for_test_with_exec_capture(
            Arc::new(AsyncMutex::new(SandboxRegistry::default())),
            Arc::new(FixedSandboxInvocationResolver::new("session-1")),
            SandboxToolProviderOptions {
                include_compat_aliases: true,
            },
            Arc::clone(&captured_exec),
        );

        let err = handler(&provider, "run_command")(invocation(
            json!({"cmd": "echo ok", "command": ["echo", "ok"]}),
            Some(trusted_context()),
        ))
        .await
        .unwrap_err();

        assert_eq!(err.code, "INVALID_ARGUMENT");
        assert!(captured_exec.lock().await.is_none());
    }

    #[tokio::test]
    async fn run_command_rejects_missing_cmd_and_command() {
        let captured_exec = Arc::new(AsyncMutex::new(None));
        let provider = SandboxToolProvider::new_for_test_with_exec_capture(
            Arc::new(AsyncMutex::new(SandboxRegistry::default())),
            Arc::new(FixedSandboxInvocationResolver::new("session-1")),
            SandboxToolProviderOptions {
                include_compat_aliases: true,
            },
            Arc::clone(&captured_exec),
        );

        let err = handler(&provider, "run_command")(invocation(
            json!({"cwd": "workspace"}),
            Some(trusted_context()),
        ))
        .await
        .unwrap_err();

        assert_eq!(err.code, "INVALID_ARGUMENT");
        assert!(captured_exec.lock().await.is_none());
    }

    #[tokio::test]
    async fn run_command_rejects_empty_command_before_runner() {
        let captured_exec = Arc::new(AsyncMutex::new(None));
        let provider = SandboxToolProvider::new_for_test_with_exec_capture(
            Arc::new(AsyncMutex::new(SandboxRegistry::default())),
            Arc::new(FixedSandboxInvocationResolver::new("session-1")),
            SandboxToolProviderOptions {
                include_compat_aliases: true,
            },
            Arc::clone(&captured_exec),
        );

        let err = handler(&provider, "run_command")(invocation(
            json!({"command": []}),
            Some(trusted_context()),
        ))
        .await
        .unwrap_err();

        assert_eq!(err.code, "INVALID_ARGUMENT");
        assert!(captured_exec.lock().await.is_none());
    }

    #[tokio::test]
    async fn import_file_resolves_host_file_ref_through_resolver() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("source.txt");
        std::fs::write(&source, "hello").unwrap();
        let registry = registry_with_session(workspace_root, SandboxPermissionMode::Readonly);
        let provider = SandboxToolProvider::new(
            Arc::clone(&registry),
            Arc::new(HostFileResolver {
                source_path: source,
                file_ref_id: None,
            }),
            SandboxToolProviderOptions {
                include_compat_aliases: true,
            },
        );
        let args = json!({"fileRefId": "public-file-1"});
        assert!(args.get("sourcePath").is_none());

        let result = handler(&provider, "import_file")(invocation(args, Some(trusted_context())))
            .await
            .unwrap();

        let sandbox_path = result["sandboxPath"].as_str().unwrap();
        assert!(sandbox_path.contains("input"));
        assert_eq!(std::fs::read_to_string(sandbox_path).unwrap(), "hello");
        let registry = registry.lock().await;
        let file_ref = registry
            .session("session-1")
            .unwrap()
            .host_file_refs
            .get("public-file-1")
            .unwrap();
        assert_eq!(file_ref.conversation_id.as_deref(), Some("run-1"));
    }

    #[tokio::test]
    async fn import_file_ensures_context_session_before_lifecycle() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("run-1");
        let source = temp.path().join("source.txt");
        std::fs::write(&source, "hello").unwrap();
        let registry = Arc::new(AsyncMutex::new(SandboxRegistry::default()));
        let provider = SandboxToolProvider::new(
            Arc::clone(&registry),
            Arc::new(EnsuringHostFileResolver {
                source_path: source,
                workspace_root: workspace_root.clone(),
            }),
            SandboxToolProviderOptions {
                include_compat_aliases: true,
            },
        );

        let result = handler(&provider, "import_file")(invocation(
            json!({"fileRefId": "public-file-1"}),
            Some(json!({
                "sessionId": "run-1",
                "runId": "run-1",
                "scopeId": "run-1",
            })),
        ))
        .await
        .unwrap();

        let sandbox_path = PathBuf::from(result["sandboxPath"].as_str().unwrap());
        assert!(sandbox_path.starts_with(workspace_root.canonicalize().unwrap().join("input")));
        assert_eq!(std::fs::read_to_string(&sandbox_path).unwrap(), "hello");
        let registry = registry.lock().await;
        assert!(registry.session("run-1").is_ok());
    }

    #[tokio::test]
    async fn import_file_safely_handles_resolver_hostile_file_ref_id() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("source.txt");
        std::fs::write(&source, "hello").unwrap();
        let registry =
            registry_with_session(workspace_root.clone(), SandboxPermissionMode::Readonly);
        let provider = SandboxToolProvider::new(
            Arc::clone(&registry),
            Arc::new(HostFileResolver {
                source_path: source,
                file_ref_id: Some("../escape".to_string()),
            }),
            SandboxToolProviderOptions {
                include_compat_aliases: true,
            },
        );
        let args = json!({"fileRefId": "public-file-1"});
        assert!(args.get("sourcePath").is_none());
        assert!(args.get("displayName").is_none());

        let result = handler(&provider, "import_file")(invocation(args, Some(trusted_context())))
            .await
            .unwrap();

        let sandbox_path = PathBuf::from(result["sandboxPath"].as_str().unwrap());
        assert!(sandbox_path.starts_with(workspace_root.canonicalize().unwrap().join("input")));
        assert_eq!(std::fs::read_to_string(&sandbox_path).unwrap(), "hello");
        assert!(!workspace_root.join("escape/source.txt").exists());
        assert_eq!(result["fileRefId"], json!("../escape"));
    }
}
