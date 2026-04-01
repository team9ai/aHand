"use client";

import type { FormEvent } from "react";
import { useState } from "react";

export default function LoginPage() {
  const [password, setPassword] = useState("");
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setIsSubmitting(true);
    setError(null);

    try {
      const response = await fetch("/api/auth/login", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ password }),
      });
      const payload = await response.json().catch(() => null);

      if (!response.ok) {
        if (response.status === 401 || response.status === 403 || payload?.error === "invalid_credentials") {
          setError("Unable to sign in with that password.");
        } else {
          setError("Unable to reach the hub right now.");
        }
        return;
      }

      window.location.href = "/";
    } catch {
      setError("Unable to reach the hub right now.");
    } finally {
      setIsSubmitting(false);
    }
  }

  return (
    <main className="auth-shell">
      <section className="auth-panel">
        <div className="auth-kicker">Secure Control Surface</div>
        <h1 className="auth-title">aHand Hub Dashboard</h1>
        <p className="auth-copy">Sign in with the shared password to reach the live device workspace.</p>
        <form className="auth-form" onSubmit={onSubmit}>
          <label className="auth-field">
            <span className="auth-label">Shared Password</span>
            <input
              className="auth-input"
              type="password"
              value={password}
              placeholder="Enter shared password"
              onChange={(event) => setPassword(event.target.value)}
            />
          </label>
          {error ? (
            <p className="auth-error" role="alert">
              {error}
            </p>
          ) : null}
          <button className="auth-submit" disabled={isSubmitting} type="submit">
            {isSubmitting ? "Signing in..." : "Sign in"}
          </button>
        </form>
      </section>
    </main>
  );
}
