# df-plugin-sdcp-v1

DragonFruit support for printers using SDCP v1 over MQTT, including the Elegoo
Saturn 3 Ultra.

## Transport

SDCP v1 differs from the WebSocket-based SDCP v3 transport:

- UDP port 3000 carries `M99999` discovery/status requests and `M66666 <port>`
  MQTT broker connection requests.
- The printer initiates an MQTT connection to DragonFruit. Control requests and
  status use `/sdcp/request/<MainboardID>`, `/sdcp/response/<MainboardID>`,
  and `/sdcp/status/<MainboardID>`.
- For file transfer, DragonFruit serves the print file over HTTP while the
  printer downloads it.

The printer and DragonFruit must be on the same reachable LAN. The desktop
firewall must allow incoming MQTT and HTTP connections on the dynamically
selected local ports.

The v1 protocol does not expose the v3 plate listing, webcam, or remote
material-profile operations. `M99999` status is used for monitoring.

## Commands

Implemented command IDs:

| ID | Operation |
| --- | --- |
| 0 | Request status |
| 1 | Request attributes |
| 128 | Start printing |
| 129 | Pause |
| 130 | Stop |
| 131 | Resume |
| 256 | Download a print file from DragonFruit |
| 512 | Configure the status update period |

## References

- [Cassini SDCP notes and Saturn 3 Ultra examples](https://github.com/vvuk/cassini)
- [Elegoo SDCP Home Assistant integration](https://github.com/danielcherubini/elegoo-homeassistant)
- [DragonFruit SDCP v3 plugin](https://github.com/Open-Resin-Alliance/df-plugin-sdcp-v3)
