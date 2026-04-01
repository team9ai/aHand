"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

const destinations = [
  { href: "/", label: "Overview" },
  { href: "/devices", label: "Devices" },
  { href: "/jobs", label: "Jobs" },
  { href: "/audit-logs", label: "Audit Logs" },
];

export function Sidebar() {
  const pathname = usePathname();

  return (
    <aside className="dashboard-rail">
      <div className="rail-brand">
        <p className="dashboard-eyebrow">aHand Hub</p>
        <h1 className="dashboard-title">Operations</h1>
        <p className="dashboard-copy">Authenticated access to device presence, jobs, and audit history.</p>
      </div>

      <nav className="rail-nav" aria-label="Dashboard sections">
        {destinations.map((destination) => {
          const active =
            pathname === destination.href ||
            (destination.href !== "/" && pathname.startsWith(destination.href));
          return (
            <Link
              className="rail-link"
              data-active={active ? "true" : "false"}
              href={destination.href}
              key={destination.href}
            >
              {destination.label}
            </Link>
          );
        })}
      </nav>

      <form action="/api/auth/logout" className="dashboard-signout" method="post">
        <button type="submit">Sign out</button>
      </form>
    </aside>
  );
}
