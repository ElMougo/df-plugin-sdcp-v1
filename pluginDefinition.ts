import type {
  ComplexPluginDefinition,
  PluginMonitoringUiAdapterContract,
  PluginNetworkUiAdapterContract,
} from '@/features/plugins/complexPluginContracts';
import { SDCP_V1_PLUGIN_MANIFEST } from './pluginManifest';
import { resolveSdcpV1MonitoringSnapshot } from './network/sdcpMonitoring';

const SDCP_V1_NETWORK_ADAPTER: PluginNetworkUiAdapterContract = {
  mode: 'sdcp-v1',
  pluginId: 'sdcp-v1',
  displayName: 'SDCP v1 (MQTT)',
  operationNamespace: 'sdcp',
  supportsRemoteMaterialProfiles: false,
  operations: {
    connect: 'sdcp/connect',
    discover: 'sdcp/discover',
    materials: 'sdcp/unsupported',
    materialsEdit: 'sdcp/unsupported',
  },
  defaultLocalHostnames: ['printer.local'],
  primaryEditFields: [],
  basicSections: [],
  advancedSections: [],
  resolveEditDraftFromMeta: () => ({}),
  resolveMaterialProcessValues: () => ({}),
  denormalizeEditDraftForBackend: () => ({}),
  resolveAdvancedSectionId: () => 'general',
  getFieldHelpText: () => 'SDCP v1 printers do not expose remote material editing.',
  isDynamicWaitEnabled: () => false,
};

const SDCP_V1_MONITORING_ADAPTER: PluginMonitoringUiAdapterContract = {
  mode: 'sdcp-v1',
  pluginId: 'sdcp-v1',
  displayName: 'SDCP v1 Monitoring',
  available: true,
  operations: {
    status: 'sdcp/printer/status',
    webcamInfo: 'sdcp/printer/webcam/info',
    platesList: 'sdcp/plates/list/json',
    start: 'sdcp/printer/start',
    deletePlate: 'sdcp/plate/delete',
    pause: 'sdcp/printer/pause',
    resume: 'sdcp/printer/unpause',
    cancel: 'sdcp/printer/stop',
    emergencyStop: 'sdcp/printer/force-stop',
  },
  parseStatusPayload: (payload: unknown) => resolveSdcpV1MonitoringSnapshot(payload),
  parseWebcamInfoPayload: () => ({
    available: false,
    streamUrl: null,
    snapshotUrl: null,
    message: 'SDCP v1 does not provide webcam endpoints.',
  }),
  getMonitoringUiPolicy: () => ({
    busyResponseGraceMs: 30_000,
    inconclusiveReachabilityMaxPolls: 2,
  }),
};

export const SDCP_V1_COMPLEX_PLUGIN_DEFINITION: ComplexPluginDefinition = {
  id: 'sdcp-v1',
  manifest: SDCP_V1_PLUGIN_MANIFEST,
  capabilities: {
    networkOperations: false,
    uploadWithProgress: true,
    slicerEncoder: false,
    tauriRuntimePlugin: true,
  },
  networkAdaptersByMode: {
    [SDCP_V1_NETWORK_ADAPTER.mode]: SDCP_V1_NETWORK_ADAPTER,
  },
  monitoringAdaptersByMode: {
    [SDCP_V1_MONITORING_ADAPTER.mode]: SDCP_V1_MONITORING_ADAPTER,
  },
};

export default SDCP_V1_COMPLEX_PLUGIN_DEFINITION;
