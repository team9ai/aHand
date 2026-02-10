import { createResource, createSignal, For, Show } from "solid-js";
import { api, RunEntry, RunDetail } from "../lib/api";

export default function RunsPanel() {
  const [offset, setOffset] = createSignal(0);
  const [selectedJobId, setSelectedJobId] = createSignal<string | null>(null);
  const [selectedFile, setSelectedFile] = createSignal<{
    jobId: string;
    filename: string;
    content: string;
  } | null>(null);

  const limit = 20;

  const [runs] = createResource(offset, (offset) =>
    api.getRuns(limit, offset)
  );

  const [runDetail] = createResource(selectedJobId, (jobId) =>
    api.getRunDetail(jobId)
  );

  function formatTimestamp(ts: number): string {
    return new Date(ts).toLocaleString();
  }

  async function handleSelectRun(jobId: string) {
    setSelectedJobId(jobId);
    setSelectedFile(null);
  }

  function handleBack() {
    setSelectedJobId(null);
    setSelectedFile(null);
  }

  async function handleViewFile(jobId: string, filename: string) {
    try {
      const content = await api.getRunFile(jobId, filename);
      setSelectedFile({ jobId, filename, content });
    } catch (e: any) {
      alert(`Failed to load file: ${e.message}`);
    }
  }

  function handleNext() {
    const current = runs();
    if (current && offset() + limit < current.total) {
      setOffset(offset() + limit);
    }
  }

  function handlePrev() {
    if (offset() >= limit) {
      setOffset(offset() - limit);
    }
  }

  return (
    <div class="panel">
      <Show
        when={selectedJobId()}
        fallback={
          <>
            <h2>Job Runs</h2>

            <Show when={runs.loading}>
              <p>Loading...</p>
            </Show>

            <Show when={runs.error}>
              <p class="error">Error: {runs.error.message}</p>
            </Show>

            <Show when={runs()}>
              {(data) => (
                <>
                  <div class="runs-info">
                    Showing {offset() + 1}-
                    {Math.min(offset() + limit, data().total)} of {data().total}
                  </div>

                  <div class="runs-list">
                    <For each={data().runs}>
                      {(run) => (
                        <div
                          class="run-item"
                          onClick={() => handleSelectRun(run.job_id)}
                        >
                          <div class="run-id">{run.job_id}</div>
                          <div class="run-time">
                            {formatTimestamp(run.created_at)}
                          </div>
                        </div>
                      )}
                    </For>
                  </div>

                  <div class="pagination">
                    <button
                      onClick={handlePrev}
                      disabled={offset() === 0}
                      class="btn-secondary"
                    >
                      Previous
                    </button>
                    <button
                      onClick={handleNext}
                      disabled={offset() + limit >= data().total}
                      class="btn-secondary"
                    >
                      Next
                    </button>
                  </div>
                </>
              )}
            </Show>
          </>
        }
      >
        <div class="panel-header">
          <h2>Run Detail: {selectedJobId()}</h2>
          <button onClick={handleBack} class="btn-secondary">
            Back to List
          </button>
        </div>

        <Show when={runDetail.loading}>
          <p>Loading...</p>
        </Show>

        <Show when={runDetail.error}>
          <p class="error">Error: {runDetail.error.message}</p>
        </Show>

        <Show when={runDetail()}>
          {(detail) => (
            <Show
              when={selectedFile()}
              fallback={
                <div class="run-detail">
                  <section>
                    <h3>Request</h3>
                    <pre>{JSON.stringify(detail().request, null, 2)}</pre>
                  </section>

                  <Show when={detail().result}>
                    <section>
                      <h3>Result</h3>
                      <pre>{JSON.stringify(detail().result, null, 2)}</pre>
                    </section>
                  </Show>

                  <section>
                    <h3>Files</h3>
                    <div class="files-list">
                      <For each={detail().files}>
                        {(filename) => (
                          <button
                            class="file-item"
                            onClick={() =>
                              handleViewFile(detail().job_id, filename)
                            }
                          >
                            {filename}
                          </button>
                        )}
                      </For>
                    </div>
                  </section>
                </div>
              }
            >
              {(file) => (
                <div class="file-view">
                  <div class="file-header">
                    <h3>{file().filename}</h3>
                    <button
                      onClick={() => setSelectedFile(null)}
                      class="btn-secondary"
                    >
                      Close
                    </button>
                  </div>
                  <pre class="file-content">{file().content}</pre>
                </div>
              )}
            </Show>
          )}
        </Show>
      </Show>
    </div>
  );
}
