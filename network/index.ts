import { pluginNetworkFetch, readNativeFileChunk, readNativeFileSize } from '@/utils/pluginNetworkBridge';

type UploadProgressEvent = {
  loaded: number;
  total: number;
  uploadSpeed: string;
  remainingTime: string;
  transferred: string;
  percentComplete: number;
};

type UploadCallbacks = {
  onProgress: (event: UploadProgressEvent) => void;
  onStatusUpdate: (update: {
    stage: 'uploading' | 'processing' | 'complete' | 'error';
    message: string;
    progress?: UploadProgressEvent;
    plateId?: number | null;
    error?: string;
  }) => void;
  onComplete?: (plateId: number | null) => void;
  onError?: (error: string) => void;
};

const CHUNK_BYTES = 1024 * 1024;
const DEFAULT_PORT = 3000;

function bytesToStringRep(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let size = bytes;
  let unitIndex = 0;
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex += 1;
  }
  return `${size.toFixed(size >= 100 || unitIndex === 0 ? 0 : 1)} ${units[unitIndex]}`;
}

function secondsToTimeString(seconds: number): string {
  const safeSeconds = Number.isFinite(seconds) ? Math.max(0, seconds) : 0;
  const h = Math.floor(safeSeconds / 3600);
  const m = Math.floor((safeSeconds % 3600) / 60);
  const s = Math.floor(safeSeconds % 60);
  if (h > 0) return `${h}h ${m}m ${s}s`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

function toHost(rawHost: string): { host: string; port: number } {
  const trimmed = rawHost.trim();
  if (!trimmed) throw new Error('Printer host is required for SDCP v1 upload.');
  const url = new URL(/^https?:\/\//i.test(trimmed) ? trimmed : `http://${trimmed}`);
  return {
    host: url.hostname,
    port: url.port ? Number(url.port) : DEFAULT_PORT,
  };
}

function toBase64(bytes: Uint8Array): string {
  let binary = '';
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
  }
  return btoa(binary);
}

function uploadId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID().replaceAll('-', '');
  }
  return Array.from({ length: 32 }, () => Math.floor(Math.random() * 16).toString(16)).join('');
}

async function post(operation: string, payload: Record<string, unknown>): Promise<Record<string, unknown>> {
  const response = await pluginNetworkFetch({
    pluginId: 'sdcp-v1',
    operation: `sdcp/${operation}`,
    ...payload,
  });
  const raw: unknown = await response.json();
  const body = raw && typeof raw === 'object' && !Array.isArray(raw)
    ? raw as Record<string, unknown>
    : {};
  if (!response.ok || body.ok === false) {
    throw new Error(typeof body.error === 'string' ? body.error : `SDCP v1 request failed (HTTP ${response.status}).`);
  }
  return body;
}

export async function uploadPrintJobWithProgress(args: {
  hostUrl: string;
  zipBlob?: Blob | null;
  zipFilePath?: string | null;
  path: string;
  profileId: string;
  callbacks: UploadCallbacks;
}): Promise<{ ok: boolean; plateId: number | null }> {
  const { hostUrl, zipBlob, zipFilePath, path, callbacks } = args;
  let phase = 'validating upload';
  try {
    const { host, port } = toHost(hostUrl);
    const nativePath = typeof zipFilePath === 'string' ? zipFilePath.trim() : '';
    const hasBlob = Boolean(zipBlob && zipBlob.size > 0);
    if (!hasBlob && !nativePath) throw new Error('No SDCP v1 upload payload is available.');

    const totalSize = hasBlob
      ? zipBlob!.size
      : await readNativeFileSize(nativePath);
    if (!Number.isFinite(totalSize) || totalSize == null || totalSize <= 0) {
      throw new Error('Could not read the SDCP v1 print file.');
    }

    const uuid = uploadId();
    const fileName = `${path.trim() || 'dragonfruit_job'}.ctb`;
    const startedAt = Date.now();
    phase = 'staging file chunks';
    callbacks.onStatusUpdate({ stage: 'uploading', message: 'Preparing print file for SDCP v1…' });

    let offset = 0;
    while (offset < totalSize) {
      const end = Math.min(totalSize, offset + CHUNK_BYTES);
      const chunk = hasBlob
        ? new Uint8Array(await zipBlob!.slice(offset, end).arrayBuffer())
        : await readNativeFileChunk(nativePath, offset, end - offset);
      if (!chunk || chunk.byteLength === 0) {
        throw new Error('Could not read an SDCP v1 upload chunk.');
      }

      await post('upload/chunk', {
        uuid,
        fileName,
        totalSize,
        offset,
        chunkBase64: toBase64(chunk),
      });

      offset += chunk.byteLength;
      const elapsedSec = Math.max(0.001, (Date.now() - startedAt) / 1000);
      const bytesPerSecond = offset / elapsedSec;
      const progress: UploadProgressEvent = {
        loaded: offset,
        total: totalSize,
        uploadSpeed: `${bytesToStringRep(bytesPerSecond)}/s`,
        remainingTime: secondsToTimeString((totalSize - offset) / Math.max(bytesPerSecond, 1)),
        transferred: `${bytesToStringRep(offset)} / ${bytesToStringRep(totalSize)}`,
        percentComplete: Math.round((offset / totalSize) * 10000) / 100,
      };
      callbacks.onProgress(progress);
      callbacks.onStatusUpdate({
        stage: offset === totalSize ? 'processing' : 'uploading',
        message: offset === totalSize ? 'Sending print file to the SDCP v1 printer…' : 'Preparing print file for SDCP v1…',
        progress,
      });
    }

    phase = 'transferring file to printer';
    const result = await post('upload/finish', { host, port, uuid });
    const plateId = typeof result.plateId === 'number' ? result.plateId : null;
    callbacks.onStatusUpdate({ stage: 'complete', message: 'SDCP v1 upload complete.', plateId });
    callbacks.onComplete?.(plateId);
    return { ok: true, plateId };
  } catch (error) {
    const message = error instanceof Error ? error.message : 'SDCP v1 upload failed.';
    console.error(`[SDCP v1] Upload failed while ${phase}`, { error: message });
    callbacks.onStatusUpdate({ stage: 'error', message: 'SDCP v1 upload failed.', error: message });
    callbacks.onError?.(message);
    throw error;
  }
}
