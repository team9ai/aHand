import { getAuditLogs } from "@/lib/api";

type AuditLogsPageProps = {
  searchParams: Promise<{
    action?: string;
    resource?: string;
    since?: string;
    until?: string;
  }>;
};

export default async function AuditLogsPage({ searchParams }: AuditLogsPageProps) {
  const filters = await searchParams;
  const auditLogs = await getAuditLogs({
    action: filters.action?.trim() || undefined,
    resource: filters.resource?.trim() || undefined,
    since: filters.since?.trim() || undefined,
    until: filters.until?.trim() || undefined,
    limit: 100,
  });

  return (
    <section className="dashboard-stack">
      <header className="dashboard-section-header">
        <div>
          <p className="dashboard-eyebrow">Audit Trail</p>
          <h1 className="dashboard-heading">Audit Logs</h1>
        </div>
        <p className="dashboard-copy">Filter operational events by action, resource, and time range.</p>
      </header>

      <form className="filter-bar filter-bar-wide" method="get">
        <input className="filter-input" defaultValue={filters.action ?? ""} name="action" placeholder="Action" />
        <input className="filter-input" defaultValue={filters.resource ?? ""} name="resource" placeholder="Resource or id" />
        <input className="filter-input" defaultValue={filters.since ?? ""} name="since" placeholder="Since (RFC3339)" />
        <input className="filter-input" defaultValue={filters.until ?? ""} name="until" placeholder="Until (RFC3339)" />
        <button className="filter-button" type="submit">
          Apply
        </button>
      </form>

      {auditLogs.length > 0 ? (
        <div className="surface-panel">
          <ul className="audit-list">
            {auditLogs.map((entry) => (
              <li className="audit-row" key={`${entry.timestamp}-${entry.action}-${entry.resource_id}`}>
                <div className="audit-summary">
                  <strong>{entry.action}</strong>
                  <span className="table-subtle">
                    {entry.resource_type}:{entry.resource_id}
                  </span>
                  <span className="table-subtle">{entry.actor}</span>
                </div>
                <details className="audit-detail" open>
                  <summary>{formatTimestamp(entry.timestamp)}</summary>
                  <pre>{JSON.stringify(entry.detail, null, 2)}</pre>
                </details>
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <p className="empty-state">No audit entries match the current filters.</p>
      )}
    </section>
  );
}

function formatTimestamp(value: string) {
  return new Intl.DateTimeFormat("en", {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(value));
}
