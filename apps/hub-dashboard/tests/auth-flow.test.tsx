import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import LoginPage from "@/app/login/page";

describe("login page", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("renders the dashboard sign-in form", () => {
    render(<LoginPage />);

    expect(screen.getByRole("heading", { name: /ahand hub/i })).toBeInTheDocument();
    expect(screen.getByLabelText(/shared password/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /sign in/i })).toBeInTheDocument();
  });

  it("shows a credential error when the shared password is rejected", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ error: "invalid_credentials" }), {
          status: 401,
          headers: { "content-type": "application/json" },
        }),
      ),
    );

    render(<LoginPage />);

    fireEvent.change(screen.getByLabelText(/shared password/i), {
      target: { value: "wrong-password" },
    });
    fireEvent.click(screen.getByRole("button", { name: /sign in/i }));

    await waitFor(() => {
      expect(screen.getByRole("alert")).toHaveTextContent("Unable to sign in with that password.");
    });
  });

  it("shows a hub availability error when the login route returns a server failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ error: "hub_unavailable" }), {
          status: 503,
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

  it("disables the submit button while the login request is in flight", async () => {
    let resolveLogin: (value: Response) => void = () => {};
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
});
