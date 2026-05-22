use serde_json::{Map, Value, json};

pub const MCP_CONFIG_ENV: &str = "AHAND_AGENT_MCP_CONFIG";
pub const MCP_CONFIG_MODE_ENV: &str = "AHAND_AGENT_MCP_CONFIG_MODE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpConfigMode {
    Merge,
    Replace,
}

impl McpConfigMode {
    pub fn from_env(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(Self::Merge),
            Some("replace") => Ok(Self::Replace),
            Some(other) => Err(format!(
                "invalid {MCP_CONFIG_MODE_ENV}: {other}; expected replace"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpConfig {
    pub value: Value,
    pub mode: McpConfigMode,
}

impl McpConfig {
    pub fn from_env(config: Option<&str>, mode: Option<&str>) -> Result<Option<Self>, String> {
        let mode = McpConfigMode::from_env(mode)?;
        let Some(raw) = config.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let value: Value = serde_json::from_str(raw)
            .map_err(|error| format!("failed to parse {MCP_CONFIG_ENV}: {error}"))?;
        validate_mcp_config(&value)?;
        Ok(Some(Self { value, mode }))
    }

    pub fn hermes_servers(&self) -> Result<Vec<Value>, String> {
        hermes_servers(&self.value)
    }
}

pub fn validate_mcp_config(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{MCP_CONFIG_ENV} must be a JSON object"))?;
    let Some(servers) = object.get("mcpServers") else {
        return Ok(());
    };
    validate_servers(servers)?;
    Ok(())
}

fn validate_servers(value: &Value) -> Result<(), String> {
    let servers = value
        .as_object()
        .ok_or_else(|| "mcpServers must be a JSON object".to_string())?;
    for (name, server) in servers {
        if name.trim().is_empty() {
            return Err("mcp server name must not be empty".to_string());
        }
        let server = server
            .as_object()
            .ok_or_else(|| format!("mcp server {name:?} must be a JSON object"))?;
        let command = server
            .get("command")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("mcp server {name:?} requires non-empty command"))?;
        let _ = command;
        if let Some(args) = server.get("args") {
            let args = args
                .as_array()
                .ok_or_else(|| format!("mcp server {name:?} args must be an array"))?;
            if args.iter().any(|arg| !arg.is_string()) {
                return Err(format!(
                    "mcp server {name:?} args must contain only strings"
                ));
            }
        }
        if let Some(env) = server.get("env") {
            let env = env
                .as_object()
                .ok_or_else(|| format!("mcp server {name:?} env must be an object"))?;
            if env.values().any(|value| !value.is_string()) {
                return Err(format!(
                    "mcp server {name:?} env values must contain only strings"
                ));
            }
        }
    }
    Ok(())
}

fn hermes_servers(value: &Value) -> Result<Vec<Value>, String> {
    let Some(servers) = value.get("mcpServers") else {
        return Ok(Vec::new());
    };
    let servers = servers
        .as_object()
        .ok_or_else(|| "mcpServers must be a JSON object".to_string())?;
    let mut names: Vec<&String> = servers.keys().collect();
    names.sort();

    let mut converted = Vec::with_capacity(names.len());
    for name in names {
        let server = servers
            .get(name)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("mcp server {name:?} must be a JSON object"))?;
        let mut item = Map::new();
        item.insert("name".to_string(), json!(name));
        item.insert(
            "command".to_string(),
            server
                .get("command")
                .cloned()
                .ok_or_else(|| format!("mcp server {name:?} requires non-empty command"))?,
        );
        item.insert(
            "args".to_string(),
            server.get("args").cloned().unwrap_or_else(|| json!([])),
        );
        item.insert("env".to_string(), hermes_env(server.get("env")));
        converted.push(Value::Object(item));
    }
    Ok(converted)
}

fn hermes_env(env: Option<&Value>) -> Value {
    let Some(env) = env.and_then(Value::as_object) else {
        return json!([]);
    };
    let mut names: Vec<&String> = env.keys().collect();
    names.sort();
    Value::Array(
        names
            .into_iter()
            .filter_map(|name| {
                env.get(name)
                    .and_then(Value::as_str)
                    .map(|value| json!({ "name": name, "value": value }))
            })
            .collect(),
    )
}

pub fn server_names(value: &Value) -> Vec<String> {
    value
        .get("mcpServers")
        .and_then(Value::as_object)
        .map(|servers| {
            let mut names: Vec<String> = servers.keys().cloned().collect();
            names.sort();
            names
        })
        .unwrap_or_default()
}
