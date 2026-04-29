import { Buffer } from "buffer";
import {
  FileRequest,
  FileResponse,
  FileListResult,
  FileMkdirResult,
  FileDeleteResult,
  FileReadTextResult,
  FileReadImageResult,
  FileReadBinaryResult,
  FileWriteResult,
  fileErrorCodeToJSON,
} from "@ahandai/proto";

// Polyfill Buffer for ts-proto generated code (Next.js 16 does not polyfill it)
if (typeof globalThis.Buffer === "undefined") {
  (globalThis as { Buffer: typeof Buffer }).Buffer = Buffer;
}

const MAX_INLINE_WRITE_BYTES = 1_048_576;

export class FileOpsError extends Error {
  readonly code: string;
  readonly httpStatus: number;
  constructor(code: string, message: string, httpStatus: number) {
    super(message);
    this.name = "FileOpsError";
    this.code = code;
    this.httpStatus = httpStatus;
  }
}

function proxyUrl(deviceId: string): string {
  return `/api/proxy/api/devices/${encodeURIComponent(deviceId)}/files`;
}

function newRequestId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `req-${Math.random().toString(36).slice(2)}-${Date.now().toString(36)}`;
}

async function sendRequest(deviceId: string, req: FileRequest): Promise<FileResponse> {
  const body = FileRequest.encode(req).finish();
  const res = await fetch(proxyUrl(deviceId), {
    method: "POST",
    headers: {
      "content-type": "application/x-protobuf",
      accept: "application/x-protobuf",
    },
    body,
    cache: "no-store",
  });
  if (!res.ok) {
    let code = `http_${res.status}`;
    let message = `Hub returned HTTP ${res.status}`;
    try {
      const envelope = (await res.clone().json()) as {
        error?: { code?: string; message?: string };
      };
      if (envelope?.error) {
        code = envelope.error.code ?? code;
        message = envelope.error.message ?? message;
      }
    } catch {
      try {
        message = (await res.text()) || message;
      } catch {
        // swallow — keep default
      }
    }
    throw new FileOpsError(code, message, res.status);
  }
  const raw = new Uint8Array(await res.arrayBuffer());
  const resp = FileResponse.decode(raw);
  if (resp.error) {
    throw new FileOpsError(
      fileErrorCodeToJSON(resp.error.code),
      resp.error.message || "file operation failed",
      200,
    );
  }
  return resp;
}

export interface ListFilesArgs {
  path: string;
  includeHidden?: boolean;
  maxResults?: number;
  offset?: number;
}

export async function listFiles(deviceId: string, args: ListFilesArgs): Promise<FileListResult> {
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    list: {
      path: args.path,
      includeHidden: args.includeHidden ?? false,
      maxResults: args.maxResults,
      offset: args.offset,
    },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.list) {
    throw new FileOpsError("malformed_response", "list response missing", 200);
  }
  return resp.list;
}

export interface ReadTextArgs {
  path: string;
  maxLines?: number;
  maxBytes?: number;
}

export async function readText(
  deviceId: string,
  args: ReadTextArgs,
): Promise<FileReadTextResult> {
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    readText: {
      path: args.path,
      maxLines: args.maxLines ?? 2000,
      maxBytes: args.maxBytes ?? 65_536,
      lineNumbers: false,
      noFollowSymlink: false,
    },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.readText) {
    throw new FileOpsError("malformed_response", "read_text response missing", 200);
  }
  return resp.readText;
}

export interface ReadImageArgs {
  path: string;
  maxBytes?: number;
}

export async function readImage(
  deviceId: string,
  args: ReadImageArgs,
): Promise<FileReadImageResult> {
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    readImage: {
      path: args.path,
      maxBytes: args.maxBytes ?? 1_048_576,
      noFollowSymlink: false,
    },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.readImage) {
    throw new FileOpsError("malformed_response", "read_image response missing", 200);
  }
  return resp.readImage;
}

export interface ReadBinaryArgs {
  path: string;
  maxBytes?: number;
}

export async function readBinary(
  deviceId: string,
  args: ReadBinaryArgs,
): Promise<FileReadBinaryResult> {
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    readBinary: {
      path: args.path,
      byteOffset: 0,
      byteLength: 0,
      maxBytes: args.maxBytes ?? MAX_INLINE_WRITE_BYTES,
      noFollowSymlink: false,
    },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.readBinary) {
    throw new FileOpsError("malformed_response", "read_binary response missing", 200);
  }
  return resp.readBinary;
}

export interface MkdirArgs {
  path: string;
  recursive?: boolean;
}

export async function mkdir(deviceId: string, args: MkdirArgs): Promise<FileMkdirResult> {
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    mkdir: { path: args.path, recursive: args.recursive ?? true },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.mkdir) {
    throw new FileOpsError("malformed_response", "mkdir response missing", 200);
  }
  return resp.mkdir;
}

export interface DeleteArgs {
  path: string;
  recursive?: boolean;
}

export async function deleteFile(
  deviceId: string,
  args: DeleteArgs,
): Promise<FileDeleteResult> {
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    delete: {
      path: args.path,
      recursive: args.recursive ?? false,
      mode: 0, // DELETE_MODE_UNSPECIFIED — daemon picks safe default
      noFollowSymlink: false,
    },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.delete) {
    throw new FileOpsError("malformed_response", "delete response missing", 200);
  }
  return resp.delete;
}

export interface WriteArgs {
  path: string;
  content: Uint8Array;
  createParents?: boolean;
}

export async function writeFile(
  deviceId: string,
  args: WriteArgs,
): Promise<FileWriteResult> {
  if (args.content.byteLength > MAX_INLINE_WRITE_BYTES) {
    throw new FileOpsError(
      "content_too_large",
      `Uploads larger than ${MAX_INLINE_WRITE_BYTES} bytes require the S3 path, which is not implemented yet.`,
      0,
    );
  }
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    write: {
      path: args.path,
      createParents: args.createParents ?? true,
      fullWrite: { content: Buffer.from(args.content) },
      noFollowSymlink: false,
    },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.write) {
    throw new FileOpsError("malformed_response", "write response missing", 200);
  }
  return resp.write;
}
