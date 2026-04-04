# Bad Case Test Coverage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add comprehensive bad case (error path, edge case, boundary condition) tests to the `codex/ahand-hub` branch across both Rust backend and TypeScript dashboard.

**Architecture:** Two independent subsystems — Rust integration/unit tests (Tasks 1-6) and TypeScript/Vitest tests (Tasks 7-12). Each task targets one test file and adds missing error/edge case coverage without modifying production code. All tests validate existing behavior.

**Tech Stack:** Rust (tokio, axum, jsonwebtoken, ed25519-dalek, prost), TypeScript (Vitest, React Testing Library, Next.js)

---

## File Structure

**Rust test files (modify only):**
- `crates/ahand-hub-core/tests/auth_service.rs` — JWT validation edge cases
- `crates/ahand-hub-core/tests/device_manager.rs` — store error propagation
- `crates/ahand-hub-core/tests/outbox.rs` — buffer overflow and sequence boundaries
- `crates/ahand-hub/tests/system_api.rs` — API input validation bad cases
- `crates/ahand-hub-core/tests/job_dispatcher.rs` — (already well-covered, skip)
- `crates/ahand-hub-core/tests/audit_service.rs` — empty/boundary filter cases

**TypeScript test files (modify only):**
- `apps/hub-dashboard/tests/dashboard-session.test.ts` — fetch failures, missing env
- `apps/hub-dashboard/tests/auth-server.test.ts` — empty token, non-JSON upstream
- `apps/hub-dashboard/tests/auth-flow.test.tsx` — network error, 500 status, empty password
- `apps/hub-dashboard/tests/dashboard-home.test.tsx` — partial API failure, hub_unavailable
- `apps/hub-dashboard/tests/devices-page.test.tsx` — device not found, API error redirect
- `apps/hub-dashboard/tests/jobs-page.test.tsx` — job not found, API error redirect
- `apps/hub-dashboard/tests/realtime-hooks.test.tsx` — WS error event, malformed JSON, finished parse error

---

### Task 1: AuthService JWT Bad Cases

**Goal:** Cover expired tokens, wrong secret, missing claims, and empty secret edge cases in `auth_service.rs`.

**Files:**
- Modify: `crates/ahand-hub-core/tests/auth_service.rs`

**Acceptance Criteria:**
- [ ] Expired JWT is rejected with `InvalidToken`
- [ ] JWT signed with a different secret is rejected
- [ ] JWT with missing `exp` claim is rejected
- [ ] AuthService constructed with empty secret still functions (issue + verify roundtrip)

**Verify:** `cargo test -p ahand-hub-core --test auth_service` → all pass

**Steps:**

- [ ] **Step 1: Add expired token test**

Append to `crates/ahand-hub-core/tests/auth_service.rs`:

```rust
#[test]
fn verify_jwt_rejects_expired_tokens() {
    let service = AuthService::new("unit-test-secret");
    let expired_token = encode(
        &Header::default(),
        &AuthContext {
            role: Role::DashboardUser,
            subject: "operator-1".into(),
            iss: "ahand-hub".into(),
            exp: (Utc::now() - Duration::hours(1)).timestamp() as usize,
        },
        &EncodingKey::from_secret("unit-test-secret".as_bytes()),
    )
    .unwrap();

    let err = service.verify_jwt(&expired_token).unwrap_err();

    assert!(matches!(err, HubError::InvalidToken(_)));
}
```

- [ ] **Step 2: Add wrong-secret test**

```rust
#[test]
fn verify_jwt_rejects_tokens_signed_with_a_different_secret() {
    let issuer = AuthService::new("secret-a");
    let verifier = AuthService::new("secret-b");
    let token = issuer.issue_dashboard_jwt("operator-1").unwrap();

    let err = verifier.verify_jwt(&token).unwrap_err();

    assert!(matches!(err, HubError::InvalidToken(_)));
}
```

- [ ] **Step 3: Add missing-exp-claim test**

```rust
#[test]
fn verify_jwt_rejects_tokens_without_exp_claim() {
    use serde_json::json;

    let service = AuthService::new("unit-test-secret");
    let header = Header::default();
    let claims = json!({
        "role": "DashboardUser",
        "subject": "operator-1",
        "iss": "ahand-hub"
    });
    let token = jsonwebtoken::encode(
        &header,
        &claims,
        &EncodingKey::from_secret("unit-test-secret".as_bytes()),
    )
    .unwrap();

    let err = service.verify_jwt(&token).unwrap_err();

    assert!(matches!(err, HubError::InvalidToken(_)));
}
```

- [ ] **Step 4: Add empty-secret roundtrip test**

```rust
#[test]
fn auth_service_with_empty_secret_still_roundtrips() {
    let service = AuthService::new("");
    let token = service.issue_dashboard_jwt("operator-1").unwrap();
    let claims = service.verify_jwt(&token).unwrap();

    assert_eq!(claims.role, Role::DashboardUser);
    assert_eq!(claims.subject, "operator-1");
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ahand-hub-core --test auth_service`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ahand-hub-core/tests/auth_service.rs
git commit -m "test(hub-core): cover JWT expiration, wrong secret, and missing claims"
```

---

### Task 2: DeviceManager Store Error Propagation

**Goal:** Cover `DeviceStore` error propagation through `DeviceManager` and empty-list edge case.

**Files:**
- Modify: `crates/ahand-hub-core/tests/device_manager.rs`

**Acceptance Criteria:**
- [ ] `list_devices` propagates `HubError::Internal` from a failing store
- [ ] `list_devices` returns empty vec for an empty store

**Verify:** `cargo test -p ahand-hub-core --test device_manager` → all pass

**Steps:**

- [ ] **Step 1: Add ErrorDeviceStore and EmptyDeviceStore**

Append to `crates/ahand-hub-core/tests/device_manager.rs`:

```rust
use ahand_hub_core::HubError;

struct ErrorDeviceStore;

#[async_trait]
impl DeviceStore for ErrorDeviceStore {
    async fn insert(&self, _device: NewDevice) -> ahand_hub_core::Result<Device> {
        Err(HubError::Internal("store unavailable".into()))
    }

    async fn get(&self, _device_id: &str) -> ahand_hub_core::Result<Option<Device>> {
        Err(HubError::Internal("store unavailable".into()))
    }

    async fn list(&self) -> ahand_hub_core::Result<Vec<Device>> {
        Err(HubError::Internal("store unavailable".into()))
    }

    async fn delete(&self, _device_id: &str) -> ahand_hub_core::Result<()> {
        Err(HubError::Internal("store unavailable".into()))
    }
}

struct EmptyDeviceStore;

#[async_trait]
impl DeviceStore for EmptyDeviceStore {
    async fn insert(&self, _device: NewDevice) -> ahand_hub_core::Result<Device> {
        unreachable!()
    }

    async fn get(&self, _device_id: &str) -> ahand_hub_core::Result<Option<Device>> {
        Ok(None)
    }

    async fn list(&self) -> ahand_hub_core::Result<Vec<Device>> {
        Ok(vec![])
    }

    async fn delete(&self, _device_id: &str) -> ahand_hub_core::Result<()> {
        Ok(())
    }
}
```

- [ ] **Step 2: Add error propagation test**

```rust
#[tokio::test]
async fn list_devices_propagates_store_errors() {
    let manager = DeviceManager::new(Arc::new(ErrorDeviceStore));

    let err = manager.list_devices().await.unwrap_err();

    assert_eq!(err, HubError::Internal("store unavailable".into()));
}
```

- [ ] **Step 3: Add empty list test**

```rust
#[tokio::test]
async fn list_devices_returns_empty_vec_for_empty_store() {
    let manager = DeviceManager::new(Arc::new(EmptyDeviceStore));

    let devices = manager.list_devices().await.unwrap();

    assert!(devices.is_empty());
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ahand-hub-core --test device_manager`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add crates/ahand-hub-core/tests/device_manager.rs
git commit -m "test(hub-core): cover device manager store errors and empty list"
```

---

### Task 3: Outbox Buffer Overflow and Sequence Boundaries

**Goal:** Cover buffer capacity eviction, replay-from boundary, and zero-capacity edge case.

**Files:**
- Modify: `crates/ahand-hub-core/tests/outbox.rs`

**Acceptance Criteria:**
- [ ] Storing beyond max_buffer evicts oldest entries
- [ ] `replay_from` with exact last_ack returns nothing for that seq
- [ ] Zero-capacity outbox immediately evicts every stored message
- [ ] `last_issued_seq` returns 0 on a fresh outbox (no underflow)

**Verify:** `cargo test -p ahand-hub-core --test outbox` → all pass

**Steps:**

- [ ] **Step 1: Add buffer overflow eviction test**

Append to `crates/ahand-hub-core/tests/outbox.rs`:

```rust
#[test]
fn outbox_evicts_oldest_when_buffer_exceeds_capacity() {
    let mut outbox = Outbox::new(2);
    outbox.store_raw(vec![1]);
    outbox.store_raw(vec![2]);
    outbox.store_raw(vec![3]);

    let replay = outbox.replay_from(0);

    assert_eq!(replay, vec![vec![2], vec![3]]);
}
```

- [ ] **Step 2: Add replay-from exact boundary test**

```rust
#[test]
fn outbox_replay_from_exact_seq_excludes_that_message() {
    let mut outbox = Outbox::new(4);
    let seq1 = outbox.store_raw(vec![10]);
    let _seq2 = outbox.store_raw(vec![20]);

    let replay = outbox.replay_from(seq1);

    assert_eq!(replay, vec![vec![20]]);
}
```

- [ ] **Step 3: Add zero-capacity test**

```rust
#[test]
fn outbox_zero_capacity_evicts_every_message_immediately() {
    let mut outbox = Outbox::new(0);
    outbox.store_raw(vec![1]);
    outbox.store_raw(vec![2]);

    assert!(outbox.is_empty());
    assert_eq!(outbox.replay_from(0), Vec::<Vec<u8>>::new());
    assert_eq!(outbox.last_issued_seq(), 2);
}
```

- [ ] **Step 4: Add fresh outbox last_issued_seq test**

```rust
#[test]
fn outbox_last_issued_seq_is_zero_on_fresh_outbox() {
    let outbox = Outbox::new(4);

    assert_eq!(outbox.last_issued_seq(), 0);
    assert!(outbox.is_empty());
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ahand-hub-core --test outbox`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ahand-hub-core/tests/outbox.rs
git commit -m "test(hub-core): cover outbox buffer overflow, boundaries, and zero capacity"
```

---

### Task 4: System API Input Validation Bad Cases

**Goal:** Add missing API-level bad case tests: empty device ID, invalid pagination values, missing content-type header.

**Files:**
- Modify: `crates/ahand-hub/tests/system_api.rs`

**Acceptance Criteria:**
- [ ] POST `/api/jobs` with missing `device_id` field returns 400 `VALIDATION_ERROR`
- [ ] POST `/api/jobs` with missing `tool` field returns 400 `VALIDATION_ERROR`
- [ ] POST `/api/devices` with missing `id` field returns 400 `VALIDATION_ERROR`
- [ ] GET `/api/devices/nonexistent` returns 404

**Verify:** `cargo test -p ahand-hub --test system_api` → all pass

**Steps:**

- [ ] **Step 1: Add missing device_id test**

Append to `crates/ahand-hub/tests/system_api.rs`:

```rust
#[tokio::test]
async fn create_job_rejects_missing_device_id() {
    let server = support::spawn_server_with_state(support::test_state().await).await;
    let response = server
        .post(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "tool": "echo",
                "args": ["hello"],
                "timeout_ms": 30_000
            }),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "VALIDATION_ERROR");
}
```

- [ ] **Step 2: Add missing tool test**

```rust
#[tokio::test]
async fn create_job_rejects_missing_tool_field() {
    let server = support::spawn_server_with_state(support::test_state().await).await;
    let response = server
        .post(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-1",
                "args": ["hello"],
                "timeout_ms": 30_000
            }),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "VALIDATION_ERROR");
}
```

- [ ] **Step 3: Add missing device id field test**

```rust
#[tokio::test]
async fn create_device_rejects_missing_id_field() {
    let server = support::spawn_server_with_state(support::test_state().await).await;
    let response = server
        .post(
            "/api/devices",
            "service-test-token",
            serde_json::json!({
                "hostname": "edge-box",
                "os": "linux",
                "capabilities": ["exec"],
                "version": "0.1.2"
            }),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "VALIDATION_ERROR");
}
```

- [ ] **Step 4: Add nonexistent device GET test**

```rust
#[tokio::test]
async fn get_device_returns_not_found_for_unknown_device() {
    let server = support::spawn_server_with_state(support::test_state().await).await;
    let response = server.get("/api/devices/device-nonexistent", "service-test-token").await;

    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ahand-hub --test system_api`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ahand-hub/tests/system_api.rs
git commit -m "test(hub): cover missing field validation and nonexistent device lookup"
```

---

### Task 5: AuditFilter Boundary Cases

**Goal:** Cover empty filter (match-all), zero limit, offset beyond entries, and empty entries input.

**Files:**
- Modify: `crates/ahand-hub-core/tests/audit_service.rs`

**Acceptance Criteria:**
- [ ] Default (empty) filter matches all entries
- [ ] `limit: 0` returns empty vec
- [ ] `offset` beyond entry count returns empty vec
- [ ] `apply` on empty input returns empty vec regardless of filters

**Verify:** `cargo test -p ahand-hub-core --test audit_service` → all pass

**Steps:**

- [ ] **Step 1: Add default filter match-all test**

Append to `crates/ahand-hub-core/tests/audit_service.rs`:

```rust
#[test]
fn default_filter_matches_all_entries() {
    let base = chrono::Utc::now();
    let entries = vec![
        AuditEntry {
            timestamp: base,
            action: "job.created".into(),
            resource_type: "job".into(),
            resource_id: "job-1".into(),
            actor: "service-a".into(),
            detail: serde_json::json!({}),
            source_ip: None,
        },
        AuditEntry {
            timestamp: base,
            action: "device.online".into(),
            resource_type: "device".into(),
            resource_id: "device-1".into(),
            actor: "service-b".into(),
            detail: serde_json::json!({}),
            source_ip: None,
        },
    ];

    let result = AuditFilter::default().apply(entries.clone());

    assert_eq!(result.len(), 2);
}
```

- [ ] **Step 2: Add zero limit test**

```rust
#[test]
fn audit_filter_zero_limit_returns_empty() {
    let entries = vec![AuditEntry {
        timestamp: chrono::Utc::now(),
        action: "job.created".into(),
        resource_type: "job".into(),
        resource_id: "job-1".into(),
        actor: "service-a".into(),
        detail: serde_json::json!({}),
        source_ip: None,
    }];

    let filter = AuditFilter {
        limit: Some(0),
        ..Default::default()
    };

    assert!(filter.apply(entries).is_empty());
}
```

- [ ] **Step 3: Add offset beyond entries test**

```rust
#[test]
fn audit_filter_offset_beyond_entries_returns_empty() {
    let entries = vec![AuditEntry {
        timestamp: chrono::Utc::now(),
        action: "job.created".into(),
        resource_type: "job".into(),
        resource_id: "job-1".into(),
        actor: "service-a".into(),
        detail: serde_json::json!({}),
        source_ip: None,
    }];

    let filter = AuditFilter {
        offset: Some(100),
        ..Default::default()
    };

    assert!(filter.apply(entries).is_empty());
}
```

- [ ] **Step 4: Add empty input test**

```rust
#[test]
fn audit_filter_apply_on_empty_input_returns_empty() {
    let filter = AuditFilter {
        resource_type: Some("job".into()),
        action: Some("job.created".into()),
        ..Default::default()
    };

    assert!(filter.apply(Vec::new()).is_empty());
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ahand-hub-core --test audit_service`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ahand-hub-core/tests/audit_service.rs
git commit -m "test(hub-core): cover audit filter boundary cases (empty, zero limit, offset overflow)"
```

---

### Task 6: Job Status Transition Backward Regression

**Goal:** Cover remaining backward transitions not yet tested: Running→Sent, Running→Pending, Cancelled→any.

**Files:**
- Modify: `crates/ahand-hub-core/src/job.rs` (test module only)

**Acceptance Criteria:**
- [ ] Running → Sent is rejected
- [ ] Running → Pending is rejected
- [ ] Cancelled → Running is rejected
- [ ] Cancelled → Pending is rejected
- [ ] Failed → Sent is rejected

**Verify:** `cargo test -p ahand-hub-core -- job::tests` → all pass

**Steps:**

- [ ] **Step 1: Add backward transition tests**

Append inside the `#[cfg(test)] mod tests` block in `crates/ahand-hub-core/src/job.rs`:

```rust
    #[test]
    fn resolve_status_transition_rejects_all_backward_transitions() {
        let backward_cases = vec![
            (JobStatus::Running, JobStatus::Sent),
            (JobStatus::Running, JobStatus::Pending),
            (JobStatus::Cancelled, JobStatus::Running),
            (JobStatus::Cancelled, JobStatus::Pending),
            (JobStatus::Cancelled, JobStatus::Sent),
            (JobStatus::Failed, JobStatus::Sent),
            (JobStatus::Failed, JobStatus::Pending),
            (JobStatus::Failed, JobStatus::Running),
            (JobStatus::Finished, JobStatus::Sent),
            (JobStatus::Finished, JobStatus::Pending),
        ];

        for (current, requested) in backward_cases {
            let result = resolve_status_transition(current, requested);
            assert_eq!(
                result,
                Err(HubError::IllegalJobTransition { current, requested }),
                "expected {current:?} -> {requested:?} to be rejected"
            );
        }
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p ahand-hub-core -- job::tests`
Expected: all PASS

- [ ] **Step 3: Commit**

```bash
git add crates/ahand-hub-core/src/job.rs
git commit -m "test(hub-core): cover all backward job status transitions"
```

---

### Task 7: Dashboard Session Verification Bad Cases

**Goal:** Cover fetch failure, missing base URL, and non-401 error responses in `verifyDashboardSession`.

**Files:**
- Modify: `apps/hub-dashboard/tests/dashboard-session.test.ts`

**Acceptance Criteria:**
- [ ] Returns null when `AHAND_HUB_BASE_URL` is missing (fetch not called)
- [ ] Returns null when fetch throws (network error)
- [ ] Returns null for non-200/non-401 responses (e.g. 500)
- [ ] Layout redirects to /login when fetch throws

**Verify:** `cd apps/hub-dashboard && npx vitest run tests/dashboard-session.test.ts` → all pass

**Steps:**

- [ ] **Step 1: Add missing base URL test**

Append inside the `describe` block in `apps/hub-dashboard/tests/dashboard-session.test.ts`:

```typescript
  it("returns null when the hub base URL is not configured", async () => {
    delete process.env.AHAND_HUB_BASE_URL;
    requestCookies.get.mockReturnValue({ value: "session-token" });

    await expect(verifyDashboardSession()).resolves.toBeNull();
    expect(fetch).not.toHaveBeenCalled();
  });
```

- [ ] **Step 2: Add fetch network error test**

```typescript
  it("returns null when the verification fetch throws a network error", async () => {
    requestCookies.get.mockReturnValue({ value: "session-token" });
    vi.mocked(fetch).mockRejectedValue(new TypeError("fetch failed"));

    await expect(verifyDashboardSession()).resolves.toBeNull();
  });
```

- [ ] **Step 3: Add non-200 non-401 response test**

```typescript
  it("returns null for server error responses from the hub", async () => {
    requestCookies.get.mockReturnValue({ value: "session-token" });
    vi.mocked(fetch).mockResolvedValue(new Response(null, { status: 500 }));

    await expect(verifyDashboardSession()).resolves.toBeNull();
  });
```

- [ ] **Step 4: Add layout redirect on fetch failure test**

```typescript
  it("redirects to login when verification fetch throws", async () => {
    requestCookies.get.mockReturnValue({ value: "session-token" });
    vi.mocked(fetch).mockRejectedValue(new TypeError("fetch failed"));

    await expect(DashboardLayout({ children: "child" })).rejects.toThrow("redirect:/login");
  });
```

- [ ] **Step 5: Run tests**

Run: `cd apps/hub-dashboard && npx vitest run tests/dashboard-session.test.ts`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add apps/hub-dashboard/tests/dashboard-session.test.ts
git commit -m "test(dashboard): cover session verification failures and missing config"
```

---

### Task 8: Auth Server Route Bad Cases

**Goal:** Cover empty token in login response, non-JSON upstream response, and login with missing base URL.

**Files:**
- Modify: `apps/hub-dashboard/tests/auth-server.test.ts`

**Acceptance Criteria:**
- [ ] Empty-string token in upstream response does not set cookies
- [ ] Non-JSON upstream response body is handled gracefully (no crash, returns empty object)
- [ ] Login returns 503 when `AHAND_HUB_BASE_URL` is missing

**Verify:** `cd apps/hub-dashboard && npx vitest run tests/auth-server.test.ts` → all pass

**Steps:**

- [ ] **Step 1: Add empty-string token test**

Append inside the `describe` block in `apps/hub-dashboard/tests/auth-server.test.ts`:

```typescript
  it("does not set cookies when the upstream login token is an empty string", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ token: "" }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      ),
    );

    const request = new NextRequest("http://localhost/api/auth/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ password: "shared-secret" }),
    });

    const response = await loginPost(request);

    expect(response.cookies.get("ahand_hub_session")).toBeUndefined();
  });
```

- [ ] **Step 2: Add non-JSON upstream response test**

```typescript
  it("handles non-JSON upstream responses without crashing", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response("<html>Bad Gateway</html>", {
          status: 502,
          headers: { "content-type": "text/html" },
        }),
      ),
    );

    const request = new NextRequest("http://localhost/api/auth/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ password: "shared-secret" }),
    });

    const response = await loginPost(request);

    expect(response.status).toBe(502);
    expect(response.cookies.get("ahand_hub_session")).toBeUndefined();
  });
```

- [ ] **Step 3: Add missing base URL for login test**

```typescript
  it("returns 503 when the login hub base URL is missing", async () => {
    delete process.env.AHAND_HUB_BASE_URL;
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    const request = new NextRequest("http://localhost/api/auth/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ password: "shared-secret" }),
    });

    const response = await loginPost(request);

    expect(response.status).toBe(503);
    await expect(response.json()).resolves.toEqual({
      error: {
        code: "hub_unavailable",
        message: "Unable to reach the hub right now.",
      },
    });
    expect(fetchMock).not.toHaveBeenCalled();
  });
```

- [ ] **Step 4: Run tests**

Run: `cd apps/hub-dashboard && npx vitest run tests/auth-server.test.ts`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add apps/hub-dashboard/tests/auth-server.test.ts
git commit -m "test(dashboard): cover empty token, non-JSON upstream, and missing login config"
```

---

### Task 9: Login Page UI Bad Cases

**Goal:** Cover network error (fetch throws), 500 server error, and form disabled state during submission.

**Files:**
- Modify: `apps/hub-dashboard/tests/auth-flow.test.tsx`

**Acceptance Criteria:**
- [ ] Network error (fetch rejects) shows "Unable to reach the hub right now."
- [ ] 500 response shows "Unable to reach the hub right now." (not credential error)
- [ ] Submit button is disabled while submitting

**Verify:** `cd apps/hub-dashboard && npx vitest run tests/auth-flow.test.tsx` → all pass

**Steps:**

- [ ] **Step 1: Add network error test**

Append inside the `describe` block in `apps/hub-dashboard/tests/auth-flow.test.tsx`:

```typescript
  it("shows a hub error when the login fetch throws a network error", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("fetch failed")));

    render(<LoginPage />);

    fireEvent.change(screen.getByLabelText(/shared password/i), {
      target: { value: "shared-secret" },
    });
    fireEvent.click(screen.getByRole("button", { name: /sign in/i }));

    await waitFor(() => {
      expect(screen.getByRole("alert")).toHaveTextContent("Unable to reach the hub right now.");
    });
  });
```

- [ ] **Step 2: Add 500 error test**

```typescript
  it("shows a hub error for 500 internal server errors", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ error: "internal" }), {
          status: 500,
          headers: { "content-type": "application/json" },
        }),
      ),
    );

    render(<LoginPage />);

    fireEvent.change(screen.getByLabelText(/shared password/i), {
      target: { value: "shared-secret" },
    });
    fireEvent.click(screen.getByRole("button", { name: /sign in/i }));

    await waitFor(() => {
      expect(screen.getByRole("alert")).toHaveTextContent("Unable to reach the hub right now.");
    });
  });
```

- [ ] **Step 3: Add submit button disabled test**

```typescript
  it("disables the submit button while the login request is in flight", async () => {
    let resolveLogin!: (value: Response) => void;
    vi.stubGlobal(
      "fetch",
      vi.fn().mockReturnValue(new Promise<Response>((resolve) => { resolveLogin = resolve; })),
    );

    render(<LoginPage />);

    fireEvent.change(screen.getByLabelText(/shared password/i), {
      target: { value: "shared-secret" },
    });
    fireEvent.click(screen.getByRole("button", { name: /sign in/i }));

    await waitFor(() => {
      expect(screen.getByRole("button")).toBeDisabled();
      expect(screen.getByRole("button")).toHaveTextContent(/signing in/i);
    });

    resolveLogin(new Response(JSON.stringify({ token: "ok" }), { status: 200 }));
  });
```

- [ ] **Step 4: Run tests**

Run: `cd apps/hub-dashboard && npx vitest run tests/auth-flow.test.tsx`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add apps/hub-dashboard/tests/auth-flow.test.tsx
git commit -m "test(dashboard): cover login network error, 500 status, and disabled submit state"
```

---

### Task 10: Dashboard Home Page Bad Cases

**Goal:** Cover API failure redirect and hub_unavailable error propagation for the overview page.

**Files:**
- Modify: `apps/hub-dashboard/tests/dashboard-home.test.tsx`

**Acceptance Criteria:**
- [ ] `getDashboardStats` throwing `hub_unavailable` propagates (not redirected)
- [ ] `getAuditLogs` failure while stats succeed still throws (Promise.all semantics)

**Verify:** `cd apps/hub-dashboard && npx vitest run tests/dashboard-home.test.tsx` → all pass

**Steps:**

- [ ] **Step 1: Add hub_unavailable error propagation test**

Append inside the `describe` block in `apps/hub-dashboard/tests/dashboard-home.test.tsx`:

```typescript
  it("propagates hub_unavailable errors without redirecting", async () => {
    vi.mocked(getDashboardStats).mockRejectedValue(new Error("hub_unavailable"));

    await expect(DashboardHomePage()).rejects.toThrow("hub_unavailable");
    expect(redirectMock).not.toHaveBeenCalled();
  });
```

- [ ] **Step 2: Add partial failure test (stats succeed, audit fails)**

```typescript
  it("throws when audit log fetch fails even if stats succeed", async () => {
    vi.mocked(getDashboardStats).mockResolvedValue({
      online_devices: 1,
      offline_devices: 0,
      running_jobs: 0,
    });
    vi.mocked(getAuditLogs).mockRejectedValue(new Error("api_500"));

    await expect(DashboardHomePage()).rejects.toThrow("api_500");
  });
```

- [ ] **Step 3: Run tests**

Run: `cd apps/hub-dashboard && npx vitest run tests/dashboard-home.test.tsx`
Expected: all PASS

- [ ] **Step 4: Commit**

```bash
git add apps/hub-dashboard/tests/dashboard-home.test.tsx
git commit -m "test(dashboard): cover home page API failures and hub_unavailable propagation"
```

---

### Task 11: Device and Job Page Bad Cases

**Goal:** Cover device-not-found rendering, API error redirect, and job-not-found rendering.

**Files:**
- Modify: `apps/hub-dashboard/tests/devices-page.test.tsx`
- Modify: `apps/hub-dashboard/tests/jobs-page.test.tsx`

**Acceptance Criteria:**
- [ ] Device detail page renders "Device not found" when `getDevice` returns null
- [ ] Devices page redirects to login on auth error
- [ ] Job detail page renders "Job not found" when `getJob` returns null
- [ ] Jobs page redirects to login on auth error

**Verify:** `cd apps/hub-dashboard && npx vitest run tests/devices-page.test.tsx tests/jobs-page.test.tsx` → all pass

**Steps:**

- [ ] **Step 1: Add device not found test**

Append inside the `describe` block in `apps/hub-dashboard/tests/devices-page.test.tsx`:

Replace the existing `vi.mock("next/navigation", ...)` at the top of `apps/hub-dashboard/tests/devices-page.test.tsx` with:

```typescript
const { redirectMock } = vi.hoisted(() => ({
  redirectMock: vi.fn((path: string) => {
    throw new Error(`REDIRECT:${path}`);
  }),
}));

vi.mock("next/navigation", () => ({
  usePathname: () => "/devices",
  redirect: redirectMock,
}));
```

Then add the tests:

```typescript
  it("renders device not found when the device does not exist", async () => {
    vi.mocked(getDevice).mockResolvedValue(null);
    vi.mocked(getJobs).mockResolvedValue([]);

    render(
      await DeviceDetailPage({
        params: Promise.resolve({ id: "device-404" }),
      }),
    );

    expect(screen.getByRole("heading", { name: /device not found/i })).toBeInTheDocument();
  });
```

- [ ] **Step 2: Add devices page auth error redirect test**

```typescript
  it("redirects to login when the devices API returns an auth error", async () => {
    vi.mocked(getDevices).mockRejectedValue(new Error("api_401"));

    await expect(
      DevicesPage({ searchParams: Promise.resolve({}) }),
    ).rejects.toThrow("REDIRECT:/login");
  });
```

- [ ] **Step 3: Add job not found test**

Append inside the `describe` block in `apps/hub-dashboard/tests/jobs-page.test.tsx`:

Add the redirect mock at the top (after existing mocks):

```typescript
const { redirectMock } = vi.hoisted(() => ({
  redirectMock: vi.fn((path: string) => {
    throw new Error(`REDIRECT:${path}`);
  }),
}));

vi.mock("next/navigation", () => ({
  redirect: redirectMock,
}));
```

Then add the test:

```typescript
  it("renders job not found when the job does not exist", async () => {
    vi.mocked(getJob).mockResolvedValue(null);
    vi.mocked(getAuditLogs).mockResolvedValue([]);
    vi.mocked(useJobOutput).mockReturnValue({
      entries: [],
      status: "idle",
      error: null,
    });

    render(
      await JobDetailPage({
        params: Promise.resolve({ id: "job-404" }),
      }),
    );

    expect(screen.getByRole("heading", { name: /job not found/i })).toBeInTheDocument();
  });
```

- [ ] **Step 4: Add jobs page auth error redirect test**

```typescript
  it("redirects to login when the jobs API returns an auth error", async () => {
    vi.mocked(getJobs).mockRejectedValue(new Error("api_401"));

    await expect(
      JobsPage({ searchParams: Promise.resolve({}) }),
    ).rejects.toThrow("REDIRECT:/login");
  });
```

- [ ] **Step 5: Run tests**

Run: `cd apps/hub-dashboard && npx vitest run tests/devices-page.test.tsx tests/jobs-page.test.tsx`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add apps/hub-dashboard/tests/devices-page.test.tsx apps/hub-dashboard/tests/jobs-page.test.tsx
git commit -m "test(dashboard): cover device/job not found and auth error redirects"
```

---

### Task 12: Realtime Hooks Bad Cases

**Goal:** Cover WebSocket error event, malformed JSON message, and EventSource finished parse error.

**Files:**
- Modify: `apps/hub-dashboard/tests/realtime-hooks.test.tsx`

**Acceptance Criteria:**
- [ ] WebSocket `error` event sets error state to `dashboard_ws_error`
- [ ] Malformed JSON in WebSocket message sets error to `dashboard_event_parse_failed`
- [ ] EventSource `finished` event with invalid JSON falls back to "Command finished"
- [ ] EventSource error while status is `complete` does not overwrite status

**Verify:** `cd apps/hub-dashboard && npx vitest run tests/realtime-hooks.test.tsx` → all pass

**Steps:**

- [ ] **Step 1: Add WebSocket error event test**

Append inside the `describe` block in `apps/hub-dashboard/tests/realtime-hooks.test.tsx`:

```typescript
  it("reports an error state when the websocket emits an error event", async () => {
    function ErrorHarness() {
      const { connectionState, error } = useDashboardWs({ reconnectDelayMs: 200 });
      return (
        <div>
          <div data-testid="state">{connectionState}</div>
          <div data-testid="error">{error ?? ""}</div>
        </div>
      );
    }

    render(<ErrorHarness />);

    act(() => {
      FakeWebSocket.instances[0]?.emit("error");
    });

    expect(screen.getByTestId("state")).toHaveTextContent("error");
    expect(screen.getByTestId("error")).toHaveTextContent("dashboard_ws_error");
  });
```

- [ ] **Step 2: Add malformed JSON test**

```typescript
  it("reports a parse error when a websocket message contains invalid JSON", async () => {
    function ParseErrorHarness() {
      const { error } = useDashboardWs({ reconnectDelayMs: 200 });
      return <div data-testid="error">{error ?? ""}</div>;
    }

    render(<ParseErrorHarness />);

    act(() => {
      FakeWebSocket.instances[0]?.emit("open");
      FakeWebSocket.instances[0]?.emit("message", { data: "not-json{{{" });
    });

    expect(screen.getByTestId("error")).toHaveTextContent("dashboard_event_parse_failed");
  });
```

- [ ] **Step 3: Add finished event parse error fallback test**

```typescript
  it("falls back to generic finished message when finished event has invalid JSON", async () => {
    render(<JobOutputHarness jobId="job-1" />);

    act(() => {
      FakeEventSource.instances[0]?.emit("finished", "not-valid-json");
    });

    expect(screen.getByTestId("entries")).toHaveTextContent("Command finished");
    expect(screen.getByTestId("status")).toHaveTextContent("complete");
  });
```

- [ ] **Step 4: Add error-after-complete does not overwrite test**

```typescript
  it("does not overwrite complete status when event source errors after finishing", async () => {
    render(<JobOutputHarness jobId="job-1" />);

    act(() => {
      FakeEventSource.instances[0]?.emit("finished", JSON.stringify({ exit_code: 0, error: "" }));
    });

    expect(screen.getByTestId("status")).toHaveTextContent("complete");

    act(() => {
      FakeEventSource.instances[0]?.onerror?.();
    });

    expect(screen.getByTestId("status")).toHaveTextContent("complete");
  });
```

- [ ] **Step 5: Run tests**

Run: `cd apps/hub-dashboard && npx vitest run tests/realtime-hooks.test.tsx`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add apps/hub-dashboard/tests/realtime-hooks.test.tsx
git commit -m "test(dashboard): cover WS error, malformed JSON, and finished parse fallback"
```
