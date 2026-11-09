import assert from 'node:assert/strict';
import test from 'node:test';
import { resolveSdcpV1MonitoringSnapshot } from './sdcpMonitoring';

test('parses SDCP v1 integer printer and print status fields', () => {
  const snapshot = resolveSdcpV1MonitoringSnapshot({
    connected: true,
    status: {
      CurrentStatus: 1,
      PrintInfo: {
        Status: 3,
        CurrentLayer: 125,
        TotalLayer: 500,
        Filename: 'sample.ctb',
      },
    },
  });

  assert.equal(snapshot.stateText, 'Printing');
  assert.equal(snapshot.isPrinting, true);
  assert.equal(snapshot.progressPct, 25);
  assert.equal(snapshot.currentLayer, 125);
  assert.equal(snapshot.totalLayers, 500);
  assert.equal(snapshot.jobName, 'sample.ctb');
});

test('recognizes paused and completed SDCP v1 print states', () => {
  const paused = resolveSdcpV1MonitoringSnapshot({
    status: { CurrentStatus: 1, PrintInfo: { Status: 7 } },
  });
  const completed = resolveSdcpV1MonitoringSnapshot({
    status: { CurrentStatus: 1, PrintInfo: { Status: 16 } },
  });

  assert.equal(paused.isPaused, true);
  assert.equal(paused.isPrinting, false);
  assert.equal(completed.finished, true);
  assert.equal(completed.stateText, 'Finished');
});

test('treats a failed status request as disconnected', () => {
  const snapshot = resolveSdcpV1MonitoringSnapshot({
    connected: false,
    error: 'timed out',
  });

  assert.equal(snapshot.connected, false);
  assert.equal(snapshot.stateText, 'Offline');
});
