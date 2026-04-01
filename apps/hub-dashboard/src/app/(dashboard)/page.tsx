import { getAuditLogs, getDashboardStats, withDashboardSession } from "@/lib/api";

export default async function DashboardHomePage() {
  const [stats, activity] = await withDashboardSession(() =>
    Promise.all([getDashboardStats(), getAuditLogs({ limit: 6 })]),
  );

  return (
    <section className="dashboard-stack">
      <header className="dashboard-section-header">
        <div>
          <p className="dashboard-eyebrow">Control Surface</p>
          <h1 className="dashboard-heading">Overview</h1>
        </div>
        <p className="dashboard-copy">
          Live operator summary across device presence, running work, and recent audit activity.
        </p>
      </header>

      <div className="stats-grid">
        <article className="stat-panel">
          <span className="stat-label">Online Devices</span>
          <strong className="stat-value">{stats.online_devices}</strong>
        </article>
        <article className="stat-panel">
          <span className="stat-label">Offline Devices</span>
          <strong className="stat-value">{stats.offline_devices}</strong>
        </article>
        <article className="stat-panel">
          <span className="stat-label">Running Jobs</span>
          <strong className="stat-value">{stats.running_jobs}</strong>
        </article>
      </div>

      <section className="surface-panel">
        <div className="panel-header">
          <h2 className="panel-title">Recent Activity</h2>
        </div>
        {activity.length > 0 ? (
          <ul className="activity-list">
            {activity.map((entry) => (
              <li className="activity-row" key={`${entry.timestamp}-${entry.action}-${entry.resource_id}`}>
                <div>
                  <strong>{entry.action}</strong>
                  <p className="dashboard-copy">
                    {entry.resource_type}:{entry.resource_id}
                  </p>
                </div>
                <time className="timestamp" dateTime={entry.timestamp}>
                  {formatTimestamp(entry.timestamp)}
                </time>
              </li>
            ))}
          </ul>
        ) : (
          <p className="empty-state">No recent activity yet.</p>
        )}
      </section>
    </section>
  );
}

function formatTimestamp(value: string) {
  return new Intl.DateTimeFormat("en", {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}
