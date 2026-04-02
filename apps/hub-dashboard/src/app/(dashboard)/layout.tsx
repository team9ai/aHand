import type { ReactNode } from "react";
import { redirect } from "next/navigation";
import { Sidebar } from "@/components/sidebar";
import { DashboardRealtimeBridge } from "@/components/dashboard-realtime-bridge";
import { verifyDashboardSession } from "@/lib/dashboard-session";

export default async function DashboardLayout({ children }: { children: ReactNode }) {
  const session = await verifyDashboardSession();

  if (!session) {
    redirect("/login");
  }

  return (
    <div className="dashboard-shell">
      <DashboardRealtimeBridge />
      <Sidebar />
      <main className="dashboard-content">{children}</main>
    </div>
  );
}
