import type { PrinterPreset } from '../../src/features/profiles/profileStore';
import elegooMarsPrinters from '../elegoo/printers/mars-series.json';
import elegooSaturnPrinters from '../elegoo/printers/saturn-series.json';

const SDCP_V1_PRESET_IDS = new Set([
  'elegoo-saturn-3-ctb',
  'elegoo-saturn-2-ctb',
  'elegoo-mars-3-ctb',
  'elegoo-mars-3-pro-ctb',
]);

const SDCP_V1_PRINTER_PRESETS = [...elegooSaturnPrinters, ...elegooMarsPrinters]
  .filter((preset) => SDCP_V1_PRESET_IDS.has(preset.presetId))
  .map((preset) => ({
    ...preset,
    imageAssetPath: `/plugins/elegoo/printers/${preset.imageAssetPath.replace(/^\.\//, '')}`,
    networkSupport: 'sdcp-v1',
  }));

export const SDCP_V1_PLUGIN_MANIFEST = {
  schemaVersion: 1,
  id: 'sdcp-v1-builtin',
  name: 'SDCP v1 Plugin',
  version: '0.1.0',
  description: 'SDCP v1 network integration for MQTT-based resin printers.',
  homepage: 'https://github.com/ElMougo/df-plugin-sdcp-v1',
  printerPresets: [
    {
      presetId: 'elegoo-saturn-3-ultra-ctb',
      profileVersion: 1,
      manufacturer: 'ELEGOO',
      family: 'Saturn',
      name: 'Saturn 3 Ultra',
      imageAssetPath: '/plugins/elegoo/printers/assets/saturn/saturn-3-ultra.png',
      antiAliasing: true,
      hasCamera: false,
      networkSupport: 'sdcp-v1',
      networkFilter: 'Saturn 3',
      buildVolumeMm: {
        width: 218.88,
        depth: 122.88,
        height: 260,
      },
      pixelSize: {
        x: 19,
        y: 24,
      },
      display: {
        resolutionX: 11520,
        resolutionY: 5120,
        outputFormat: '.ctb',
        formatVersion: 'v5enc',
        settingsMode: 'twostage',
        mirrorX: true,
        mirrorY: false,
      },
    } satisfies PrinterPreset,
    ...SDCP_V1_PRINTER_PRESETS,
  ],
  materialTemplates: [],
};
