import { DeviceStatusBadge } from "@/components/device-status-badge";
import { DeviceTabs } from "@/components/device-tabs";
import { getDevice, getJobs, withDashboardSession } from "@/lib/api";

type DeviceDetailPageProps = {
  params: Promise<{ id: string }>;
};

export default async function DeviceDetailPage({ params }: DeviceDetailPageProps) {
  const { id } = await params;
  const [device, jobs] = await withDashboardSession(() =>
    Promise.all([getDevice(id), getJobs({ deviceId: id })]),
  );

  if (!device) {
    return (
      <section className="dashboard-stack">
        <h1 className="dashboard-heading">Device not found</h1>
        <p className="empty-state">The requested device could not be loaded from the hub.</p>
      </section>
    );
  }

  return (
    <section className="dashboard-stack">
      <header className="dashboard-section-header">
        <div>
          <p className="dashboard-eyebrow">Device Detail</p>
          <h1 className="dashboard-heading">{device.hostname}</h1>
        </div>
        <DeviceStatusBadge online={device.online} />
      </header>

      <div className="detail-grid">
        <article className="surface-panel">
          <h2 className="panel-title">Metadata</h2>
          <dl className="detail-list">
            <div>
              <dt>Device ID</dt>
              <dd>{device.id}</dd>
            </div>
            <div>
              <dt>OS</dt>
              <dd>{device.os}</dd>
            </div>
            <div>
              <dt>Version</dt>
              <dd>{device.version ?? "Unknown"}</dd>
            </div>
            <div>
              <dt>Auth Method</dt>
              <dd>{device.auth_method}</dd>
            </div>
            <div>
              <dt>Public Key Fingerprint</dt>
              <dd>{formatFingerprint(device.public_key)}</dd>
            </div>
          </dl>
        </article>

        <article className="surface-panel">
          <h2 className="panel-title">Capabilities</h2>
          <ul className="tag-list">
            {device.capabilities.map((capability) => (
              <li className="tag-pill" key={capability}>
                {capability}
              </li>
            ))}
          </ul>
        </article>
      </div>

      <DeviceTabs deviceId={device.id} jobs={jobs} online={device.online} />
    </section>
  );
}

function formatFingerprint(publicKey: number[] | null) {
  if (!publicKey || publicKey.length === 0) {
    return "Unavailable";
  }

  const hex = publicKey
    .map((value) => value.toString(16).padStart(2, "0"))
    .join("")
    .toUpperCase();

  return hex.replace(/.{8}/g, "$& ").trim();
}
