use std::collections::HashMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::process::Child;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};
use url::Url;

use crate::browser::BrowserCommandResult;
use crate::config::BrowserConfig;

#[derive(Debug, Clone)]
pub struct CdpPageSession {
    pub endpoint: String,
    pub target_id: String,
    pub web_socket_debugger_url: String,
}

#[derive(Default)]
pub struct CdpRuntimeState {
    pub endpoint: Option<String>,
    pub launched_child: Option<Child>,
    pub sessions: HashMap<String, CdpPageSession>,
}

pub struct CdpProvider<'a> {
    config: &'a BrowserConfig,
}

impl<'a> CdpProvider<'a> {
    pub fn new(config: &'a BrowserConfig) -> Self {
        Self { config }
    }

    pub async fn execute(
        &self,
        state: &mut CdpRuntimeState,
        session_id: &str,
        action: &str,
        params_json: &str,
        timeout_ms: u64,
    ) -> BrowserCommandResult {
        match self
            .execute_inner(state, session_id, action, params_json, timeout_ms)
            .await
        {
            Ok(result) => result,
            Err(err) => BrowserCommandResult {
                success: false,
                error: err.to_string(),
                ..Default::default()
            },
        }
    }

    async fn execute_inner(
        &self,
        state: &mut CdpRuntimeState,
        session_id: &str,
        action: &str,
        params_json: &str,
        timeout_ms: u64,
    ) -> anyhow::Result<BrowserCommandResult> {
        let params = parse_params(params_json)?;
        if action == "close" {
            self.close_session(state, session_id).await?;
            return Ok(BrowserCommandResult {
                success: true,
                result_json: serde_json::json!({ "closed": true }).to_string(),
                ..Default::default()
            });
        }

        if action == "wait" {
            return self
                .execute_wait(state, session_id, &params, timeout_ms)
                .await;
        }

        let page = self.ensure_page(state, session_id, &params).await?;
        match action {
            "open" | "goto" => {
                let url = params
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("url required for browser action '{action}'"))?;
                let result = cdp_command(
                    &page.web_socket_debugger_url,
                    "Page.navigate",
                    serde_json::json!({ "url": url }),
                    timeout_ms,
                )
                .await?;
                Ok(success_json(result))
            }
            "click" => {
                let selector = selector_param(&params)?;
                let expr = format!(
                    r#"(() => {{
const el = document.querySelector({});
if (!el) throw new Error("selector not found: " + {});
el.scrollIntoView({{ block: "center", inline: "center" }});
el.click();
return true;
}})()"#,
                    json_string(&selector),
                    json_string(&selector)
                );
                self.evaluate_expression(&page, &expr, timeout_ms).await
            }
            "fill" => {
                let selector = selector_param(&params)?;
                let text = params
                    .get("text")
                    .or_else(|| params.get("value"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let expr = format!(
                    r#"(() => {{
const el = document.querySelector({});
if (!el) throw new Error("selector not found: " + {});
el.focus();
el.value = {};
el.dispatchEvent(new Event("input", {{ bubbles: true }}));
el.dispatchEvent(new Event("change", {{ bubbles: true }}));
return true;
}})()"#,
                    json_string(&selector),
                    json_string(&selector),
                    json_string(text)
                );
                self.evaluate_expression(&page, &expr, timeout_ms).await
            }
            "type" => {
                let text = params
                    .get("text")
                    .or_else(|| params.get("value"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let expr = if let Ok(selector) = selector_param(&params) {
                    format!(
                        r#"(() => {{
const el = document.querySelector({});
if (!el) throw new Error("selector not found: " + {});
el.focus();
el.value = (el.value || "") + {};
el.dispatchEvent(new Event("input", {{ bubbles: true }}));
return true;
}})()"#,
                        json_string(&selector),
                        json_string(&selector),
                        json_string(text)
                    )
                } else {
                    format!(
                        r#"(() => {{
const el = document.activeElement;
if (!el) throw new Error("no active element");
el.value = (el.value || "") + {};
el.dispatchEvent(new Event("input", {{ bubbles: true }}));
return true;
}})()"#,
                        json_string(text)
                    )
                };
                self.evaluate_expression(&page, &expr, timeout_ms).await
            }
            "eval" => {
                let expression = params
                    .get("expression")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("expression required for browser eval"))?;
                self.evaluate_expression(&page, expression, timeout_ms)
                    .await
            }
            other => bail!("browser provider 'cdp' does not support action '{other}'"),
        }
    }

    async fn execute_wait(
        &self,
        state: &mut CdpRuntimeState,
        session_id: &str,
        params: &Value,
        timeout_ms: u64,
    ) -> anyhow::Result<BrowserCommandResult> {
        if let Some(text) = params.get("text").and_then(Value::as_str) {
            if !text.is_empty() {
                let page = self.ensure_page(state, session_id, params).await?;
                let effective_timeout = effective_timeout(timeout_ms, self.config);
                let deadline = Instant::now() + Duration::from_millis(effective_timeout);
                let poll_interval = Duration::from_millis(500);
                let expr = format!(
                    "() => document.body && document.body.innerText.includes({})",
                    json_string(text)
                );
                loop {
                    let result = self.evaluate_expression(&page, &expr, 10_000).await?;
                    if result.result_json.trim() == "true" {
                        return Ok(BrowserCommandResult {
                            success: true,
                            result_json: format!("Text '{}' found on page", text),
                            ..Default::default()
                        });
                    }
                    if Instant::now() + poll_interval > deadline {
                        bail!("Timeout: text '{text}' not found within {effective_timeout}ms");
                    }
                    tokio::time::sleep(poll_interval).await;
                }
            }
        }

        let delay_ms = params
            .get("timeMs")
            .or_else(|| params.get("timeout"))
            .or_else(|| params.get("timeout_ms"))
            .and_then(Value::as_u64)
            .unwrap_or(1000);
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        Ok(BrowserCommandResult {
            success: true,
            result_json: serde_json::json!({ "waited_ms": delay_ms }).to_string(),
            ..Default::default()
        })
    }

    async fn ensure_page(
        &self,
        state: &mut CdpRuntimeState,
        session_id: &str,
        params: &Value,
    ) -> anyhow::Result<CdpPageSession> {
        let target_mode = params
            .get("target")
            .and_then(|v| v.get("mode"))
            .and_then(Value::as_str)
            .unwrap_or("session_tab");

        if target_mode == "session_tab" {
            if let Some(page) = state.sessions.get(session_id) {
                return Ok(page.clone());
            }
        } else if target_mode != "new_tab" {
            bail!("unsupported CDP target.mode '{target_mode}'");
        }

        let endpoint = self.ensure_endpoint(state).await?;
        let initial_url = params
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("about:blank");
        let page = create_target(&endpoint, initial_url).await?;
        state.sessions.insert(session_id.to_string(), page.clone());
        Ok(page)
    }

    async fn ensure_endpoint(&self, state: &mut CdpRuntimeState) -> anyhow::Result<String> {
        if let Some(configured) = self.config.cdp_endpoint.as_deref() {
            if !configured.trim().is_empty() {
                let endpoint = normalize_endpoint(configured)?;
                ensure_loopback_endpoint(&endpoint)?;
                probe_endpoint(&endpoint).await?;
                return Ok(endpoint);
            }
        }

        if let Some(endpoint) = state.endpoint.clone() {
            if probe_endpoint(&endpoint).await.is_ok() {
                return Ok(endpoint);
            }
            state.endpoint = None;
            state.launched_child = None;
            state.sessions.clear();
        }

        let endpoint = self.launch_browser(state).await?;
        state.endpoint = Some(endpoint.clone());
        Ok(endpoint)
    }

    async fn launch_browser(&self, state: &mut CdpRuntimeState) -> anyhow::Result<String> {
        let executable = resolve_executable_path(self.config)?;
        let port = allocate_local_port()?;
        let endpoint = format!("http://127.0.0.1:{port}");

        let mut command = tokio::process::Command::new(&executable);
        command
            .arg(format!("--remote-debugging-port={port}"))
            .arg("--remote-debugging-address=127.0.0.1")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let child = command.spawn().with_context(|| {
            format!(
                "failed to launch Chrome for CDP at {}",
                executable.to_string_lossy()
            )
        })?;
        state.launched_child = Some(child);

        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if probe_endpoint(&endpoint).await.is_ok() {
                info!(endpoint, "launched Chrome CDP endpoint");
                return Ok(endpoint);
            }
            if Instant::now() > deadline {
                bail!(
                    "launched Chrome but CDP endpoint {endpoint} did not become ready; if Chrome is already running without remote debugging, close it or configure browser.cdp_endpoint"
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn evaluate_expression(
        &self,
        page: &CdpPageSession,
        expression: &str,
        timeout_ms: u64,
    ) -> anyhow::Result<BrowserCommandResult> {
        let result = cdp_command(
            &page.web_socket_debugger_url,
            "Runtime.evaluate",
            serde_json::json!({
                "expression": expression,
                "awaitPromise": true,
                "returnByValue": true,
                "userGesture": true
            }),
            timeout_ms,
        )
        .await?;

        if let Some(exception) = result.get("exceptionDetails") {
            bail!("CDP eval exception: {exception}");
        }

        let value = result
            .get("result")
            .and_then(|v| v.get("value"))
            .cloned()
            .unwrap_or(Value::Null);
        Ok(BrowserCommandResult {
            success: true,
            result_json: value_to_result_json(value),
            ..Default::default()
        })
    }

    pub async fn close_session(
        &self,
        state: &mut CdpRuntimeState,
        session_id: &str,
    ) -> anyhow::Result<()> {
        if let Some(page) = state.sessions.remove(session_id) {
            let _ = close_target(&page.endpoint, &page.target_id).await;
        }

        if state.sessions.is_empty() {
            if let Some(mut child) = state.launched_child.take() {
                if let Err(err) = child.kill().await {
                    warn!(error = %err, "failed to kill aHand-launched Chrome CDP process");
                }
            }
            state.endpoint = None;
        }
        Ok(())
    }
}

async fn cdp_command(
    web_socket_debugger_url: &str,
    method: &str,
    params: Value,
    timeout_ms: u64,
) -> anyhow::Result<Value> {
    let timeout = Duration::from_millis(if timeout_ms > 0 { timeout_ms } else { 30_000 });
    tokio::time::timeout(
        timeout,
        cdp_command_inner(web_socket_debugger_url, method, params),
    )
    .await
    .map_err(|_| anyhow!("CDP command '{method}' timed out"))?
}

async fn cdp_command_inner(
    web_socket_debugger_url: &str,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    let (mut ws, _) = tokio_tungstenite::connect_async(web_socket_debugger_url)
        .await
        .with_context(|| format!("failed to connect CDP websocket {web_socket_debugger_url}"))?;
    let id = 1_u64;
    ws.send(Message::Text(
        serde_json::json!({ "id": id, "method": method, "params": params }).to_string(),
    ))
    .await?;

    while let Some(message) = ws.next().await {
        let message = message?;
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Message::Close(_) => bail!("CDP websocket closed before response for '{method}'"),
            _ => continue,
        };
        let value: Value = serde_json::from_str(&text)?;
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            bail!("CDP command '{method}' failed: {error}");
        }
        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
    }

    bail!("CDP websocket ended before response for '{method}'")
}

async fn probe_endpoint(endpoint: &str) -> anyhow::Result<Value> {
    let url = format!("{}/json/version", endpoint.trim_end_matches('/'));
    let client = cdp_http_client(Duration::from_millis(800))?;
    let value = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(value)
}

async fn create_target(endpoint: &str, initial_url: &str) -> anyhow::Result<CdpPageSession> {
    let encoded_url: String =
        url::form_urlencoded::byte_serialize(initial_url.as_bytes()).collect();
    let url = format!(
        "{}/json/new?{}",
        endpoint.trim_end_matches('/'),
        encoded_url
    );
    let client = cdp_http_client(Duration::from_secs(10))?;
    let response = match client.put(&url).send().await {
        Ok(resp) if resp.status().is_success() => resp,
        _ => client.get(&url).send().await?,
    };
    let value: Value = response.error_for_status()?.json().await?;
    let target_id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("CDP /json/new response missing target id"))?
        .to_string();
    let web_socket_debugger_url = value
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("CDP /json/new response missing webSocketDebuggerUrl"))?
        .to_string();
    Ok(CdpPageSession {
        endpoint: endpoint.to_string(),
        target_id,
        web_socket_debugger_url,
    })
}

async fn close_target(endpoint: &str, target_id: &str) -> anyhow::Result<()> {
    let url = format!(
        "{}/json/close/{}",
        endpoint.trim_end_matches('/'),
        url::form_urlencoded::byte_serialize(target_id.as_bytes()).collect::<String>()
    );
    let _ = cdp_http_client(Duration::from_secs(10))?
        .get(url)
        .send()
        .await?;
    Ok(())
}

fn cdp_http_client(timeout: Duration) -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .timeout(timeout)
        .build()?)
}

fn parse_params(params_json: &str) -> anyhow::Result<Value> {
    if params_json.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(params_json).context("invalid browser params_json")
}

fn selector_param(params: &Value) -> anyhow::Result<String> {
    params
        .get("selector")
        .or_else(|| params.get("ref"))
        .or_else(|| params.get("element_ref"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("selector required for CDP browser action"))
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn value_to_result_json(value: Value) -> String {
    match value {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

fn success_json(value: Value) -> BrowserCommandResult {
    BrowserCommandResult {
        success: true,
        result_json: value.to_string(),
        ..Default::default()
    }
}

fn effective_timeout(timeout_ms: u64, config: &BrowserConfig) -> u64 {
    if timeout_ms > 0 {
        timeout_ms
    } else {
        config.default_timeout_ms.unwrap_or(30_000)
    }
}

fn allocate_local_port() -> anyhow::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn resolve_executable_path(config: &BrowserConfig) -> anyhow::Result<PathBuf> {
    crate::browser_setup::detect_browser(config.executable_path.as_deref())
        .map(|browser| browser.path)
        .ok_or_else(|| anyhow!("no supported Chrome/Chromium browser is available for CDP"))
}

fn normalize_endpoint(raw: &str) -> anyhow::Result<String> {
    let parsed = Url::parse(raw).context("invalid browser.cdp_endpoint")?;
    let scheme = match parsed.scheme() {
        "http" | "https" => parsed.scheme().to_string(),
        "ws" => "http".to_string(),
        "wss" => "https".to_string(),
        other => bail!("unsupported browser.cdp_endpoint scheme '{other}'"),
    };
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("browser.cdp_endpoint missing host"))?;
    let host_for_origin = if host.starts_with('[') || !host.contains(':') {
        host.to_string()
    } else {
        format!("[{host}]")
    };
    let mut endpoint = format!("{scheme}://{host_for_origin}");
    if let Some(port) = parsed.port() {
        endpoint.push_str(&format!(":{port}"));
    }
    Ok(endpoint)
}

fn ensure_loopback_endpoint(endpoint: &str) -> anyhow::Result<()> {
    let parsed = Url::parse(endpoint)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("browser.cdp_endpoint missing host"))?;
    if matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") {
        Ok(())
    } else {
        bail!("browser.cdp_endpoint must point to a localhost endpoint")
    }
}

pub fn endpoint_is_loopback(raw: &str) -> bool {
    normalize_endpoint(raw)
        .and_then(|endpoint| ensure_loopback_endpoint(&endpoint))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener as TokioTcpListener;

    #[test]
    fn normalize_ws_endpoint_to_http_origin() {
        assert_eq!(
            normalize_endpoint("ws://127.0.0.1:9222/devtools/browser/abc").unwrap(),
            "http://127.0.0.1:9222"
        );
    }

    #[test]
    fn normalize_ipv6_loopback_endpoint_to_http_origin() {
        assert_eq!(
            normalize_endpoint("ws://[::1]:9222/devtools/browser/abc").unwrap(),
            "http://[::1]:9222"
        );
        assert!(endpoint_is_loopback("ws://[::1]:9222/devtools/browser/abc"));
    }

    #[test]
    fn reject_non_loopback_endpoint() {
        let err = ensure_loopback_endpoint("http://192.168.1.20:9222").unwrap_err();
        assert!(err.to_string().contains("localhost"));
    }

    #[test]
    fn selector_accepts_selector_ref_or_element_ref() {
        assert_eq!(
            selector_param(&serde_json::json!({ "selector": "#q" })).unwrap(),
            "#q"
        );
        assert_eq!(
            selector_param(&serde_json::json!({ "ref": "[name=q]" })).unwrap(),
            "[name=q]"
        );
        assert_eq!(
            selector_param(&serde_json::json!({ "element_ref": ".item" })).unwrap(),
            ".item"
        );
    }

    #[tokio::test]
    async fn explicit_endpoint_executes_open_eval_and_close() {
        let ws_listener = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = ws_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    while let Some(message) = ws.next().await {
                        let Ok(message) = message else {
                            break;
                        };
                        let Message::Text(text) = message else {
                            continue;
                        };
                        let request: Value = serde_json::from_str(&text).unwrap();
                        let id = request.get("id").and_then(Value::as_u64).unwrap();
                        let method = request.get("method").and_then(Value::as_str).unwrap();
                        let result = match method {
                            "Page.navigate" => serde_json::json!({ "frameId": "frame-1" }),
                            "Runtime.evaluate" => serde_json::json!({
                                "result": { "type": "number", "value": 2 }
                            }),
                            other => serde_json::json!({
                                "result": { "type": "string", "value": format!("unsupported {other}") }
                            }),
                        };
                        ws.send(Message::Text(
                            serde_json::json!({ "id": id, "result": result }).to_string(),
                        ))
                        .await
                        .unwrap();
                    }
                });
            }
        });

        let http_listener = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_addr = http_listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = http_listener.accept().await else {
                    break;
                };
                let mut buf = Vec::new();
                loop {
                    let mut chunk = [0_u8; 1024];
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&buf);
                let body = if request.starts_with("GET /json/version") {
                    serde_json::json!({ "Browser": "FakeChrome/1" }).to_string()
                } else if request.starts_with("PUT /json/new")
                    || request.starts_with("GET /json/new")
                {
                    serde_json::json!({
                        "id": "target-1",
                        "webSocketDebuggerUrl": format!("ws://{}/devtools/page/target-1", ws_addr)
                    })
                    .to_string()
                } else if request.starts_with("GET /json/close/target-1") {
                    "Target is closing".to_string()
                } else {
                    "{}".to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let config = BrowserConfig {
            cdp_endpoint: Some(format!("http://{}", http_addr)),
            ..BrowserConfig::default()
        };
        let provider = CdpProvider::new(&config);
        let mut state = CdpRuntimeState::default();

        let open = provider
            .execute(
                &mut state,
                "session-1",
                "open",
                r#"{"url":"https://example.com"}"#,
                5_000,
            )
            .await;
        assert!(open.success, "open failed: {}", open.error);

        let eval = provider
            .execute(
                &mut state,
                "session-1",
                "eval",
                r#"{"expression":"1 + 1"}"#,
                5_000,
            )
            .await;
        assert!(eval.success, "eval failed: {}", eval.error);
        assert_eq!(eval.result_json, "2");

        let close = provider
            .execute(&mut state, "session-1", "close", "{}", 5_000)
            .await;
        assert!(close.success, "close failed: {}", close.error);
        assert!(state.sessions.is_empty());
    }
}
