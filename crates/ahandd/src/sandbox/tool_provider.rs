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
        CommitResult, FileVersion, HostFileRef, RegisterVersionRequest, RuntimeExecuteResult,
        SandboxError, SandboxExecRequest, SandboxExecResult, SandboxFile, SandboxResult,
    },
};

pub const CODE_SANDBOX_CONTEXT_REQUIRED: &str = "SANDBOX_CONTEXT_REQUIRED";

#[derive(Debug, Clone, Copy)]
pub struct SandboxToolProviderOptions {
    pub include_compat_aliases: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxInvocationContext {
    pub session_id: String,
    pub run_id: Option<String>,
    pub scope_id: Option<String>,
}

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
        })
    }
}

#[derive(Clone)]
pub struct SandboxToolProvider {
    registry: Arc<AsyncMutex<SandboxRegistry>>,
    resolver: Arc<dyn SandboxInvocationResolver>,
    options: SandboxToolProviderOptions,
    #[cfg(test)]
    captured_exec: Option<Arc<AsyncMutex<Option<SandboxExecRequest>>>>,
}

impl SandboxToolProvider {
    pub fn new(
        registry: Arc<AsyncMutex<SandboxRegistry>>,
        resolver: Arc<dyn SandboxInvocationResolver>,
        options: SandboxToolProviderOptions,
    ) -> Self {
        Self {
            registry,
            resolver,
            options,
            #[cfg(test)]
            captured_exec: None,
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
            captured_exec: Some(captured_exec),
        }
    }

    pub fn tool_handlers(&self) -> Vec<(AppToolDef, AppToolHandler)> {
        let mut tools = vec![
            (import_file_def(), self.import_file_handler()),
            (
                run_command_def("run_command"),
                self.run_command_handler("run_command"),
            ),
            (
                register_file_version_def(),
                self.register_file_version_handler(),
            ),
            (
                commit_file_version_def(),
                self.commit_file_version_handler(),
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

    fn register_file_version_handler(&self) -> AppToolHandler {
        let provider = self.clone();
        Arc::new(move |invocation| {
            let provider = provider.clone();
            let future: BoxFuture<'static, Result<Value, AppToolError>> =
                Box::pin(async move { provider.register_file_version(invocation).await });
            future
        })
    }

    fn commit_file_version_handler(&self) -> AppToolHandler {
        let provider = self.clone();
        Arc::new(move |invocation| {
            let provider = provider.clone();
            let future: BoxFuture<'static, Result<Value, AppToolError>> =
                Box::pin(async move { provider.commit_file_version(invocation).await });
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

        let file = {
            let mut registry = self.registry.lock().await;
            file_lifecycle::import_file(&mut registry, &context.session_id, host_file_ref)
        }
        .map_err(app_tool_error_from_sandbox)?;

        Ok(sandbox_file_json(&file))
    }

    async fn run_command(&self, invocation: AppToolInvocation) -> Result<Value, AppToolError> {
        let context = self
            .resolver
            .resolve(&invocation)
            .map_err(app_tool_error_from_sandbox)?;
        let command = require_non_empty_string_array_arg(&invocation.args, "command")?;
        let cwd = optional_string_arg(&invocation.args, "cwd")?.map(PathBuf::from);
        let env = optional_string_map_arg(&invocation.args, "env")?;
        let timeout = optional_timeout_arg(&invocation.args, "timeoutSeconds")?;
        let result = self
            .execute_command(
                &context.session_id,
                SandboxExecRequest {
                    command,
                    cwd,
                    env,
                    timeout,
                },
            )
            .await
            .map_err(app_tool_error_from_sandbox)?;

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
        let command = std::iter::once("node".to_string()).chain(args).collect();
        let result = self
            .execute_command(
                &context.session_id,
                SandboxExecRequest {
                    command,
                    cwd,
                    env: HashMap::new(),
                    timeout,
                },
            )
            .await
            .map_err(app_tool_error_from_sandbox)?;

        Ok(runtime_execute_result_json(&result))
    }

    async fn register_file_version(
        &self,
        invocation: AppToolInvocation,
    ) -> Result<Value, AppToolError> {
        let context = self
            .resolver
            .resolve(&invocation)
            .map_err(app_tool_error_from_sandbox)?;
        let sandbox_path = require_string_arg(&invocation.args, "sandboxPath")?;
        let source_file_ref_id = optional_string_arg(&invocation.args, "sourceFileRefId")?;

        let version = {
            let mut registry = self.registry.lock().await;
            file_lifecycle::register_file_version(
                &mut registry,
                &context.session_id,
                RegisterVersionRequest {
                    sandbox_path: PathBuf::from(sandbox_path),
                    source_file_ref_id,
                },
            )
        }
        .map_err(app_tool_error_from_sandbox)?;

        Ok(file_version_json(&version))
    }

    async fn commit_file_version(
        &self,
        invocation: AppToolInvocation,
    ) -> Result<Value, AppToolError> {
        let context = self
            .resolver
            .resolve(&invocation)
            .map_err(app_tool_error_from_sandbox)?;
        let version_id = require_string_arg(&invocation.args, "versionId")?;

        let result = {
            let mut registry = self.registry.lock().await;
            file_lifecycle::commit_file_version(&mut registry, &context.session_id, &version_id)
        }
        .map_err(app_tool_error_from_sandbox)?;

        Ok(commit_result_json(&result))
    }

    async fn execute_command(
        &self,
        session_id: &str,
        request: SandboxExecRequest,
    ) -> SandboxResult<SandboxExecResult> {
        #[cfg(test)]
        if let Some(captured_exec) = &self.captured_exec {
            *captured_exec.lock().await = Some(request);
            return Ok(RuntimeExecuteResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: Some(0),
                timed_out: false,
            });
        }

        crate::public_api::execute_sandbox_command_with_registry(
            Arc::clone(&self.registry),
            session_id,
            request,
        )
        .await
    }
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
        return Err(invalid_arg(name, "must be an integer from 1 to 120"));
    };
    if !(1..=120).contains(&seconds) {
        return Err(invalid_arg(name, "must be an integer from 1 to 120"));
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

fn file_version_json(version: &FileVersion) -> Value {
    json!({
        "versionId": version.version_id,
        "sandboxPath": version.sandbox_path.to_string_lossy().to_string(),
        "sourceFileRefId": version.source_file_ref_id,
        "size": version.size,
        "hash": version.hash,
        "status": serde_json::to_value(&version.status)
            .expect("file version status serializes to JSON"),
    })
}

fn commit_result_json(result: &CommitResult) -> Value {
    json!({
        "versionId": result.version_id,
        "sourceFileRefId": result.source_file_ref_id,
        "backupId": result.backup_id,
        "oldHash": result.old_hash,
        "newHash": result.new_hash,
        "bytesWritten": result.bytes_written,
        "permissionMode": serde_json::to_value(result.permission_mode)
            .expect("sandbox permission mode serializes to JSON"),
        "permissionVersion": result.permission_version,
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
                    "maximum": 120
                }
            },
            "required": ["command"],
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
                    "maximum": 120
                }
            },
            "required": ["args"],
            "additionalProperties": false
        }),
        requires_approval: false,
    }
}

fn register_file_version_def() -> AppToolDef {
    AppToolDef {
        name: "register_file_version".to_string(),
        description: "Register a sandbox file as a candidate file version".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "sandboxPath": { "type": "string" },
                "sourceFileRefId": { "type": "string" }
            },
            "required": ["sandboxPath"],
            "additionalProperties": false
        }),
        requires_approval: false,
    }
}

fn commit_file_version_def() -> AppToolDef {
    AppToolDef {
        name: "commit_file_version".to_string(),
        description: "Commit a sandbox file version back to its source file".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "versionId": { "type": "string" }
            },
            "required": ["versionId"],
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
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::sync::Mutex as AsyncMutex;

    use super::*;
    use crate::app_tool_registry::AppToolInvocation;
    use crate::sandbox::registry::SandboxRegistry;
    use crate::sandbox::types::{
        HostFileRef, NetworkPolicy, SandboxExecRequest, SandboxPermissionMode, SandboxSessionConfig,
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

    #[test]
    fn provider_registers_run_command_and_compat_aliases() {
        let names = tool_names(&provider(true));

        assert!(names.contains("run_command"));
        assert!(names.contains("sandbox_exec"));
        assert!(names.contains("run_node"));
        assert!(names.contains("import_file"));
        assert!(names.contains("register_file_version"));
        assert!(names.contains("commit_file_version"));
    }

    #[test]
    fn provider_can_disable_compat_aliases() {
        let names = tool_names(&provider(false));

        assert!(names.contains("run_command"));
        assert!(!names.contains("sandbox_exec"));
        assert!(!names.contains("run_node"));
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
        assert_eq!(captured.command, vec!["node", "script.js"]);
        assert_eq!(captured.cwd, Some(PathBuf::from("workspace")));
        assert_eq!(captured.timeout, Some(Duration::from_secs(7)));
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

    #[tokio::test]
    async fn register_and_commit_file_handlers_call_lifecycle() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("source.txt");
        std::fs::write(&source, "original").unwrap();
        let registry = registry_with_session(workspace_root.clone(), SandboxPermissionMode::Full);
        let provider = SandboxToolProvider::new(
            Arc::clone(&registry),
            Arc::new(HostFileResolver {
                source_path: source.clone(),
                file_ref_id: None,
            }),
            SandboxToolProviderOptions {
                include_compat_aliases: true,
            },
        );
        let import_result = handler(&provider, "import_file")(invocation(
            json!({"fileRefId": "public-file-1"}),
            Some(trusted_context()),
        ))
        .await
        .unwrap();
        let sandbox_path = PathBuf::from(import_result["sandboxPath"].as_str().unwrap());
        let sandbox_relative_path = sandbox_path
            .strip_prefix(workspace_root.canonicalize().unwrap())
            .unwrap()
            .to_string_lossy()
            .to_string();
        std::fs::write(&sandbox_path, "updated").unwrap();

        let version = handler(&provider, "register_file_version")(invocation(
            json!({
                "sandboxPath": sandbox_relative_path,
                "sourceFileRefId": "public-file-1"
            }),
            Some(trusted_context()),
        ))
        .await
        .unwrap();

        assert_eq!(version["status"], json!("candidate"));
        let commit = handler(&provider, "commit_file_version")(invocation(
            json!({"versionId": version["versionId"].as_str().unwrap()}),
            Some(trusted_context()),
        ))
        .await
        .unwrap();

        assert_eq!(commit["sourceFileRefId"], json!("public-file-1"));
        assert_eq!(commit["bytesWritten"], json!(7));
        assert_eq!(std::fs::read_to_string(source).unwrap(), "updated");
    }
}
