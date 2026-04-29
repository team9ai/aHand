"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { FileType, ImageFormat, type FileEntry } from "@ahandai/proto";
import {
  FileOpsError,
  deleteFile,
  listFiles,
  mkdir,
  readBinary,
  readImage,
  readText,
  writeFile,
} from "@/lib/file-ops-client";

interface ViewerState {
  kind: "text" | "image" | "binary" | "loading";
  path: string;
  text?: string;
  imageSrc?: string;
  imageMime?: string;
}

interface ErrorState {
  code: string;
  message: string;
}

export function DeviceFiles({ deviceId }: { deviceId: string }) {
  const [path, setPath] = useState("/tmp");
  const [entries, setEntries] = useState<FileEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<ErrorState | null>(null);
  const [viewer, setViewer] = useState<ViewerState | null>(null);

  const openDirectory = useCallback(
    async (target: string) => {
      setLoading(true);
      setError(null);
      setViewer(null);
      try {
        const result = await listFiles(deviceId, { path: target });
        const sorted = [...result.entries].sort((a, b) => {
          const aDir = a.fileType === FileType.FILE_TYPE_DIRECTORY ? 0 : 1;
          const bDir = b.fileType === FileType.FILE_TYPE_DIRECTORY ? 0 : 1;
          if (aDir !== bDir) return aDir - bDir;
          return a.name.localeCompare(b.name);
        });
        setEntries(sorted);
        setPath(target);
      } catch (e) {
        setEntries(null);
        setError(toErrorState(e));
      } finally {
        setLoading(false);
      }
    },
    [deviceId],
  );

  const openFile = useCallback(
    async (entryPath: string, entryName: string) => {
      setError(null);
      setViewer({ kind: "loading", path: entryPath });
      if (isImage(entryName)) {
        try {
          const r = await readImage(deviceId, { path: entryPath });
          const mime = imageMimeFor(r.format);
          const src = `data:${mime};base64,${uint8ToBase64(r.content)}`;
          setViewer({ kind: "image", path: entryPath, imageSrc: src, imageMime: mime });
        } catch (e) {
          setViewer(null);
          setError(toErrorState(e));
        }
        return;
      }
      try {
        const r = await readText(deviceId, { path: entryPath });
        const joined = r.lines.map((l) => l.content).join("\n");
        // Daemon decodes even binary files via chardetng and returns text,
        // so a proto-level ENCODING error is never what we want to check.
        // NUL bytes in the joined string are the strongest client-side signal
        // that the file is not really text.
        if (joined.includes("\0")) {
          setViewer({ kind: "binary", path: entryPath });
          return;
        }
        setViewer({ kind: "text", path: entryPath, text: joined });
      } catch (e) {
        setViewer(null);
        setError(toErrorState(e));
      }
    },
    [deviceId],
  );

  const [mkdirName, setMkdirName] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<{ name: string; recursive: boolean } | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    if (!pendingDelete) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPendingDelete(null);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [pendingDelete]);

  const handleMkdir = useCallback(async () => {
    if (mkdirName === null) return;
    const name = mkdirName.trim();
    if (!name) return;
    setBusy("mkdir");
    setError(null);
    try {
      await mkdir(deviceId, { path: joinPath(path, name), recursive: true });
      setMkdirName(null);
      await openDirectory(path);
    } catch (e) {
      setError(toErrorState(e));
    } finally {
      setBusy(null);
    }
  }, [mkdirName, deviceId, path, openDirectory]);

  const handleDelete = useCallback(async () => {
    if (!pendingDelete) return;
    const { name, recursive } = pendingDelete;
    setBusy("delete");
    setError(null);
    try {
      await deleteFile(deviceId, { path: joinPath(path, name), recursive });
      setPendingDelete(null);
      await openDirectory(path);
    } catch (e) {
      setError(toErrorState(e));
    } finally {
      setBusy(null);
    }
  }, [pendingDelete, deviceId, path, openDirectory]);

  const handleDownload = useCallback(
    async (name: string) => {
      setBusy(`download:${name}`);
      setError(null);
      try {
        const r = await readBinary(deviceId, { path: joinPath(path, name) });
        const view = new Uint8Array(r.content);
        const blob = new Blob([view], { type: "application/octet-stream" });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = name;
        document.body.appendChild(a);
        a.click();
        a.remove();
        // Safari drops the download if we revoke synchronously — let the
        // download dialog attach first.
        setTimeout(() => URL.revokeObjectURL(url), 0);
      } catch (e) {
        setError(toErrorState(e));
      } finally {
        setBusy(null);
      }
    },
    [deviceId, path],
  );

  const handleUpload = useCallback(
    async (file: File | null | undefined) => {
      if (!file) return;
      setBusy("upload");
      setError(null);
      try {
        if (file.size > 1_048_576) {
          throw new FileOpsError(
            "content_too_large",
            "Uploads larger than 1 MiB require the S3 path, which is not implemented yet.",
            0,
          );
        }
        const bytes = new Uint8Array(await file.arrayBuffer());
        await writeFile(deviceId, { path: joinPath(path, file.name), content: bytes });
        await openDirectory(path);
      } catch (e) {
        setError(toErrorState(e));
      } finally {
        setBusy(null);
      }
    },
    [deviceId, path, openDirectory],
  );

  const crumbs = useMemo(() => buildBreadcrumbs(path), [path]);

  return (
    <div className="files-panel">
      <div className="files-section">
        <div className="files-form-row">
          <label className="files-label" htmlFor="files-path-input">
            Path
          </label>
          <input
            id="files-path-input"
            className="files-input"
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder="/home/user"
            onKeyDown={(e) => e.key === "Enter" && openDirectory(path)}
          />
          <button
            type="button"
            className="files-btn files-btn-primary"
            onClick={() => openDirectory(path)}
            disabled={loading}
          >
            {loading ? "Loading..." : "Open"}
          </button>
        </div>
        <nav aria-label="Directory breadcrumbs" className="files-breadcrumbs">
          {crumbs.map((c, i) => (
            <button
              key={`${c.path}-${i}`}
              type="button"
              className="files-breadcrumb"
              onClick={() => openDirectory(c.path)}
            >
              {c.label}
            </button>
          ))}
        </nav>
        <div className="files-actions-row">
          <button
            type="button"
            className="files-btn"
            onClick={() => setMkdirName("")}
            disabled={busy !== null}
          >
            New folder
          </button>
          <label className="files-btn files-upload-label">
            Upload file
            <input
              type="file"
              className="files-upload-input"
              aria-label="Upload file"
              disabled={busy !== null}
              onChange={(e) => {
                const file = e.target.files?.[0];
                e.target.value = "";
                handleUpload(file);
              }}
            />
          </label>
        </div>
        {mkdirName !== null && (
          <div className="files-form-row">
            <label className="files-label" htmlFor="files-mkdir-input">
              Folder name
            </label>
            <input
              id="files-mkdir-input"
              className="files-input"
              value={mkdirName}
              onChange={(e) => setMkdirName(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleMkdir()}
            />
            <button
              type="button"
              className="files-btn files-btn-primary"
              onClick={handleMkdir}
              disabled={busy === "mkdir"}
            >
              Create
            </button>
            <button
              type="button"
              className="files-btn"
              onClick={() => setMkdirName(null)}
            >
              Cancel
            </button>
          </div>
        )}
      </div>

      {error && (
        <div className="files-error" role="alert">
          <strong>{error.code}</strong>: {error.message}
        </div>
      )}

      {entries && (
        <ul className="files-list" aria-label="Directory entries">
          {entries.length === 0 && (
            <li className="files-empty">(empty directory)</li>
          )}
          {entries.map((e) => (
            <li key={e.name} className="files-entry">
              <button
                type="button"
                className="files-entry-btn"
                onClick={() =>
                  e.fileType === FileType.FILE_TYPE_DIRECTORY
                    ? openDirectory(joinPath(path, e.name))
                    : openFile(joinPath(path, e.name), e.name)
                }
                aria-label={`${typeLabel(e.fileType)} ${e.name}`}
              >
                <span className={`files-badge files-badge-${typeLabel(e.fileType).toLowerCase()}`}>
                  {typeLabel(e.fileType)}
                </span>
                <span className="files-entry-name">{e.name}</span>
                <span className="files-entry-size">
                  {e.fileType === FileType.FILE_TYPE_FILE ? humanSize(e.size) : ""}
                </span>
                <span className="files-entry-time">{formatMtime(e.modifiedMs)}</span>
              </button>
              <div className="files-entry-actions">
                {e.fileType === FileType.FILE_TYPE_FILE && (
                  <button
                    type="button"
                    className="files-btn files-btn-sm"
                    onClick={() => handleDownload(e.name)}
                    disabled={busy === `download:${e.name}`}
                    aria-label={`Download ${e.name}`}
                  >
                    Download
                  </button>
                )}
                <button
                  type="button"
                  className="files-btn files-btn-sm files-btn-danger"
                  onClick={() => setPendingDelete({ name: e.name, recursive: false })}
                  disabled={busy === "delete"}
                  aria-label={`Delete ${e.name}`}
                >
                  Delete
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}

      {viewer && (
        <div className="files-viewer" role="region" aria-label={`Viewer for ${viewer.path}`}>
          <div className="files-viewer-header">
            <span className="files-viewer-path">{viewer.path}</span>
            <button
              type="button"
              className="files-btn files-btn-sm"
              onClick={() => setViewer(null)}
            >
              Close
            </button>
          </div>
          {viewer.kind === "loading" && (
            <p className="files-viewer-loading">Loading...</p>
          )}
          {viewer.kind === "text" && (
            <pre className="files-viewer-text">{viewer.text}</pre>
          )}
          {viewer.kind === "image" && (
            <img
              className="files-viewer-image"
              src={viewer.imageSrc}
              alt={viewer.path}
            />
          )}
          {viewer.kind === "binary" && (
            <p className="files-viewer-binary">
              Binary file — use Download to save it locally.
            </p>
          )}
        </div>
      )}
      {pendingDelete && (
        <div className="files-dialog-backdrop">
          <div
            className="files-dialog"
            role="dialog"
            aria-modal="true"
            aria-label={`Delete ${pendingDelete.name}`}
          >
            <h3 className="files-dialog-title">Delete {pendingDelete.name}?</h3>
            <label className="files-dialog-option">
              <input
                type="checkbox"
                checked={pendingDelete.recursive}
                onChange={(e) =>
                  setPendingDelete({
                    name: pendingDelete.name,
                    recursive: e.target.checked,
                  })
                }
              />
              Recursive (delete contents)
            </label>
            <div className="files-dialog-actions">
              <button
                type="button"
                className="files-btn"
                onClick={() => setPendingDelete(null)}
              >
                Cancel
              </button>
              <button
                type="button"
                className="files-btn files-btn-danger"
                onClick={handleDelete}
                disabled={busy === "delete"}
              >
                Delete
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function toErrorState(e: unknown): ErrorState {
  if (e instanceof FileOpsError) return { code: e.code, message: e.message };
  if (e instanceof Error) return { code: "ClientError", message: e.message };
  return { code: "ClientError", message: String(e) };
}

function typeLabel(t: FileType): "DIR" | "FILE" | "LNK" | "OTHER" {
  switch (t) {
    case FileType.FILE_TYPE_DIRECTORY:
      return "DIR";
    case FileType.FILE_TYPE_FILE:
      return "FILE";
    case FileType.FILE_TYPE_SYMLINK:
      return "LNK";
    default:
      return "OTHER";
  }
}

function humanSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MiB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(1)} GiB`;
}

function formatMtime(ms: number): string {
  if (!ms) return "";
  return new Date(ms).toLocaleString();
}

function joinPath(base: string, name: string): string {
  if (base === "/") return `/${name}`;
  if (base.endsWith("/")) return `${base}${name}`;
  return `${base}/${name}`;
}

function buildBreadcrumbs(p: string): { label: string; path: string }[] {
  const trimmed = p.replace(/\/+$/, "");
  if (!trimmed || trimmed === "") {
    return [{ label: "/", path: "/" }];
  }
  const isAbs = trimmed.startsWith("/");
  const parts = trimmed.split("/").filter(Boolean);
  const crumbs: { label: string; path: string }[] = [];
  if (isAbs) crumbs.push({ label: "/", path: "/" });
  let cursor = "";
  for (const part of parts) {
    cursor = isAbs ? `${cursor}/${part}` : cursor ? `${cursor}/${part}` : part;
    crumbs.push({ label: part, path: cursor || "/" });
  }
  return crumbs;
}

function isImage(name: string): boolean {
  return /\.(png|jpe?g|gif|webp|bmp|ico|svg)$/i.test(name);
}

function imageMimeFor(fmt: ImageFormat): string {
  switch (fmt) {
    case ImageFormat.IMAGE_FORMAT_JPEG: return "image/jpeg";
    case ImageFormat.IMAGE_FORMAT_PNG: return "image/png";
    case ImageFormat.IMAGE_FORMAT_WEBP: return "image/webp";
    default: return "image/png";
  }
}

function uint8ToBase64(bytes: Uint8Array): string {
  let s = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    s += String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK) as unknown as number[]);
  }
  return btoa(s);
}
