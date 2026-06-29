use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;

use crate::app_tool_registry::{AppToolDef, AppToolError, AppToolHandler, AppToolInvocation};
use crate::sandbox::registry::SandboxRegistry;
use crate::sandbox::types::{SandboxError, SandboxResult};

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

pub struct SandboxToolProvider {
    registry: Arc<AsyncMutex<SandboxRegistry>>,
    resolver: Arc<dyn SandboxInvocationResolver>,
    options: SandboxToolProviderOptions,
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
        self.unavailable_handler("import_file")
    }

    fn run_command_handler(&self, tool_name: &'static str) -> AppToolHandler {
        self.unavailable_handler(tool_name)
    }

    fn register_file_version_handler(&self) -> AppToolHandler {
        self.unavailable_handler("register_file_version")
    }

    fn commit_file_version_handler(&self) -> AppToolHandler {
        self.unavailable_handler("commit_file_version")
    }

    fn run_node_handler(&self) -> AppToolHandler {
        self.unavailable_handler("run_node")
    }

    fn unavailable_handler(&self, tool_name: &'static str) -> AppToolHandler {
        let _registry = Arc::clone(&self.registry);
        let _resolver = Arc::clone(&self.resolver);
        unavailable_handler(tool_name)
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
                    "items": { "type": "string" }
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

fn unavailable_handler(tool_name: &'static str) -> AppToolHandler {
    Arc::new(move |_invocation| {
        let future: BoxFuture<'static, Result<Value, AppToolError>> = Box::pin(async move {
            Err(AppToolError {
                code: "SANDBOX_UNAVAILABLE".to_string(),
                message: format!("sandbox tool '{tool_name}' is not configured"),
            })
        });
        future
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use serde_json::{Value, json};
    use tokio::sync::Mutex as AsyncMutex;

    use super::*;
    use crate::app_tool_registry::AppToolInvocation;
    use crate::sandbox::registry::SandboxRegistry;

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

    fn invocation(args: Value, context: Option<Value>) -> AppToolInvocation {
        AppToolInvocation {
            tool_call_id: "call-1".to_string(),
            name: "run_command".to_string(),
            args,
            timeout_ms: 5_000,
            context,
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
}
