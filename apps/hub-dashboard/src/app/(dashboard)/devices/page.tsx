import Link from "next/link";
import { DeviceStatusBadge } from "@/components/device-status-badge";
import { getDevices } from "@/lib/api";

type DevicesPageProps = {
  searchParams: Promise<{
    status?: string;
    q?: string;
  }>;
};

export default async function DevicesPage({ searchParams }: DevicesPageProps) {
  const [{ status, q }, devices] = await Promise.all([searchParams, getDevices()]);
  const normalizedQuery = q?.trim().toLowerCase() ?? "";
  const filteredDevices = devices.filter((device) => {
    const matchesStatus =
      !status ||
      status === "all" ||
      (status === "online" && device.online) ||
      (status === "offline" && !device.online);
    const matchesQuery =
      normalizedQuery.length === 0 ||
      device.hostname.toLowerCase().includes(normalizedQuery) ||
      device.id.toLowerCase().includes(normalizedQuery);
    return matchesStatus && matchesQuery;
  });

  return (
    <section className="dashboard-stack">
      <header className="dashboard-section-header">
        <div>
          <p className="dashboard-eyebrow">Inventory</p>
          <h1 className="dashboard-heading">Devices</h1>
        </div>
        <p className="dashboard-copy">Presence-aware device inventory with direct access to metadata and recent jobs.</p>
      </header>

      <form className="filter-bar" method="get">
        <input className="filter-input" defaultValue={q ?? ""} name="q" placeholder="Search hostname or device id" />
        <select className="filter-select" defaultValue={status ?? "all"} name="status">
          <option value="all">All statuses</option>
          <option value="online">Online only</option>
          <option value="offline">Offline only</option>
        </select>
        <button className="filter-button" type="submit">
          Apply
        </button>
      </form>

      {filteredDevices.length > 0 ? (
        <div className="surface-panel">
          <table className="data-table">
            <thead>
              <tr>
                <th>Device</th>
                <th>OS</th>
                <th>Capabilities</th>
                <th>Status</th>
              </tr>
            </thead>
            <tbody>
              {filteredDevices.map((device) => (
                <tr key={device.id}>
                  <td>
                    <Link className="table-link" href={`/devices/${device.id}`}>
                      {device.hostname}
                    </Link>
                    <div className="table-subtle">{device.id}</div>
                  </td>
                  <td>{device.os}</td>
                  <td>{device.capabilities.join(", ") || "None"}</td>
                  <td>
                    <DeviceStatusBadge online={device.online} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <p className="empty-state">No devices match the current filters.</p>
      )}
    </section>
  );
}
