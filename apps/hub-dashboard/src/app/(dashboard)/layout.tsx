import type { ReactNode } from "react";
import { Sidebar } from "@/components/sidebar";
import { DashboardRealtimeBridge } from "@/components/dashboard-realtime-bridge";

export default function DashboardLayout({ children }: { children: ReactNode }) {
  return (
    <div className="dashboard-shell">
      <DashboardRealtimeBridge />
      <Sidebar />
      <main className="dashboard-content">{children}</main>
    </div>
  );
}
