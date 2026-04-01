import { JobOutputViewer } from "@/components/job-output-viewer";
import { getAuditLogs, getJob, withDashboardSession } from "@/lib/api";

type JobDetailPageProps = {
  params: Promise<{ id: string }>;
};

export default async function JobDetailPage({ params }: JobDetailPageProps) {
  const { id } = await params;
  const [job, timeline] = await withDashboardSession(() =>
    Promise.all([getJob(id), getAuditLogs({ resource: id, limit: 20 })]),
  );

  if (!job) {
    return (
      <section className="dashboard-stack">
        <h1 className="dashboard-heading">Job not found</h1>
        <p className="empty-state">The requested job could not be loaded from the hub.</p>
      </section>
    );
  }

  const jobTimeline = timeline
    .filter((entry) => entry.resource_type === "job")
    .sort((left, right) => left.timestamp.localeCompare(right.timestamp));

  return (
    <section className="dashboard-stack">
      <header className="dashboard-section-header">
        <div>
          <p className="dashboard-eyebrow">Job Detail</p>
          <h1 className="dashboard-heading">Job {job.id}</h1>
        </div>
        <span className="status-badge" data-online="true">
          {job.status.toLowerCase()}
        </span>
      </header>

      <div className="detail-grid">
        <article className="surface-panel">
          <h2 className="panel-title">Metadata</h2>
          <dl className="detail-list">
            <div>
              <dt>Tool</dt>
              <dd>{job.tool}</dd>
            </div>
            <div>
              <dt>Device</dt>
              <dd>{job.device_id}</dd>
            </div>
            <div>
              <dt>Working Directory</dt>
              <dd>{job.cwd ?? "Default"}</dd>
            </div>
            <div>
              <dt>Timeout</dt>
              <dd>{Math.round(job.timeout_ms / 1000)}s</dd>
            </div>
          </dl>
        </article>

        <article className="surface-panel">
          <h2 className="panel-title">State Timeline</h2>
          {jobTimeline.length > 0 ? (
            <ol className="timeline-list">
              {jobTimeline.map((entry) => (
                <li className="timeline-row" key={`${entry.timestamp}-${entry.action}`}>
                  <strong>{entry.action}</strong>
                  <span className="table-subtle">{formatTimestamp(entry.timestamp)}</span>
                </li>
              ))}
            </ol>
          ) : (
            <p className="empty-state">No timeline events have been recorded yet.</p>
          )}
        </article>
      </div>

      <JobOutputViewer jobId={job.id} />
    </section>
  );
}

function formatTimestamp(value: string) {
  return new Intl.DateTimeFormat("en", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(value));
}
