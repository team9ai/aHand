import Link from "next/link";
import { getJobs, withDashboardSession } from "@/lib/api";

type JobsPageProps = {
  searchParams: Promise<{
    status?: string;
    device?: string;
  }>;
};

export default async function JobsPage({ searchParams }: JobsPageProps) {
  const { status, device } = await searchParams;
  const jobs = await withDashboardSession(() =>
    getJobs({
      status: status && status !== "all" ? status : undefined,
      deviceId: device?.trim() || undefined,
    }),
  );
  const filteredJobs = jobs.filter((job) => {
    const matchesStatus = !status || status === "all" || job.status.toLowerCase() === status;
    const matchesDevice = !device || device.trim().length === 0 || job.device_id === device.trim();
    return matchesStatus && matchesDevice;
  });

  return (
    <section className="dashboard-stack">
      <header className="dashboard-section-header">
        <div>
          <p className="dashboard-eyebrow">Execution</p>
          <h1 className="dashboard-heading">Jobs</h1>
        </div>
        <p className="dashboard-copy">Inspect status transitions, requested tools, and device affinity for each job.</p>
      </header>

      <form className="filter-bar" method="get">
        <input className="filter-input" defaultValue={device ?? ""} name="device" placeholder="Filter by device id" />
        <select className="filter-select" defaultValue={status ?? "all"} name="status">
          <option value="all">All statuses</option>
          <option value="pending">Pending</option>
          <option value="sent">Sent</option>
          <option value="running">Running</option>
          <option value="finished">Finished</option>
          <option value="failed">Failed</option>
          <option value="cancelled">Cancelled</option>
        </select>
        <button className="filter-button" type="submit">
          Apply
        </button>
      </form>

      {filteredJobs.length > 0 ? (
        <div className="surface-panel">
          <table className="data-table">
            <thead>
              <tr>
                <th>Tool</th>
                <th>Device</th>
                <th>Arguments</th>
                <th>Status</th>
              </tr>
            </thead>
            <tbody>
              {filteredJobs.map((job) => (
                <tr key={job.id}>
                  <td>
                    <Link className="table-link" href={`/jobs/${job.id}`}>
                      {job.tool}
                    </Link>
                  </td>
                  <td>{job.device_id}</td>
                  <td>{job.args.join(" ") || "No arguments"}</td>
                  <td>{job.status.toLowerCase()}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <p className="empty-state">No jobs found for the current filters.</p>
      )}
    </section>
  );
}
