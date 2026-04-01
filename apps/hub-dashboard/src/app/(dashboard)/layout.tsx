import type { ReactNode } from "react";

export default function DashboardLayout({ children }: { children: ReactNode }) {
  return (
    <div className="dashboard-shell">
      <aside className="dashboard-rail">
        <h1 className="dashboard-title">aHand Hub</h1>
        <p className="dashboard-copy">Authenticated operators land here before Task 8 adds the live device and jobs views.</p>
        <form action="/api/auth/logout" className="dashboard-signout" method="post">
          <button type="submit">Sign out</button>
        </form>
      </aside>
      <main className="dashboard-content">{children}</main>
    </div>
  );
}
