type RecordValue = Record<string, unknown>;

export type SdcpV1MonitoringSnapshot = {
  connected: boolean;
  stateText: string;
  isPrinting: boolean;
  isPaused: boolean;
  cancelLatched: boolean;
  pauseLatched: boolean;
  finished: boolean;
  progressPct: number | null;
  currentLayer: number | null;
  totalLayers: number | null;
  plateId: number | null;
  jobName: string | null;
  etaSec: number | null;
};

function record(value: unknown): RecordValue {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as RecordValue
    : {};
}

function finiteNumber(value: unknown): number | null {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function nonEmptyString(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value.trim() : null;
}

export function resolveSdcpV1MonitoringSnapshot(payload: unknown): SdcpV1MonitoringSnapshot {
  const root = record(payload);
  const status = record(root.status ?? record(root.Data).Status);
  const printInfo = record(status.PrintInfo);
  const currentStatus = finiteNumber(status.CurrentStatus);
  const printStatus = finiteNumber(printInfo.Status);
  const currentLayer = finiteNumber(printInfo.CurrentLayer);
  const totalLayers = finiteNumber(printInfo.TotalLayer ?? printInfo.TotalLayers);
  const isPaused = printStatus === 7;
  const finished = printStatus === 16 || printStatus === 9;
  const connected = root.connected === true || root.ok === true || currentStatus != null;
  const isPrinting = connected && currentStatus === 1 && !isPaused && !finished;
  const progressPct = currentLayer != null && totalLayers != null && totalLayers > 0
    ? Math.max(0, Math.min(100, (currentLayer / totalLayers) * 100))
    : null;
  const stateText = !connected
    ? 'Offline'
    : isPaused
      ? 'Paused'
      : finished
        ? 'Finished'
        : isPrinting
          ? 'Printing'
          : 'Online';

  return {
    connected,
    stateText,
    isPrinting,
    isPaused,
    cancelLatched: false,
    pauseLatched: false,
    finished,
    progressPct,
    currentLayer: currentLayer != null && currentLayer >= 0 ? Math.round(currentLayer) : null,
    totalLayers: totalLayers != null && totalLayers > 0 ? Math.round(totalLayers) : null,
    plateId: null,
    jobName: nonEmptyString(printInfo.Filename),
    etaSec: null,
  };
}
