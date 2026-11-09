use base64::Engine;
use md5::{Digest, Md5};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DISCOVERY_PORT: u16 = 3000;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_MQTT_PACKET_BYTES: usize = 4 * 1024 * 1024;
static UPLOADS: OnceLock<Mutex<HashMap<String, UploadEntry>>> = OnceLock::new();
static PRINT_FILES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Serialize)]
pub struct PluginNetworkResponse {
    pub status: u16,
    pub body: Value,
}

struct UploadEntry {
    file_path: PathBuf,
    file_name: String,
    total_size: u64,
    written: u64,
}

struct DiscoveryResponse {
    id: String,
    mainboard_id: String,
    data: Value,
}

struct MqttSession {
    stream: TcpStream,
    mainboard_id: String,
    connection_id: String,
}

fn uploads() -> &'static Mutex<HashMap<String, UploadEntry>> {
    UPLOADS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn print_files() -> &'static Mutex<HashMap<String, String>> {
    PRINT_FILES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn request_id() -> String {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:032x}", time ^ u128::from(REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)))
}

fn payload_string<'a>(payload: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn parse_ipv4_host(host: &str) -> Result<Ipv4Addr, String> {
    let trimmed = host.trim();
    if let Ok(address) = trimmed.parse::<Ipv4Addr>() {
        return Ok(address);
    }
    (trimmed, DISCOVERY_PORT)
        .to_socket_addrs()
        .map_err(|error| format!("Could not resolve SDCP v1 printer host: {error}"))?
        .find_map(|address| match address.ip() {
            IpAddr::V4(address) => Some(address),
            IpAddr::V6(_) => None,
        })
        .ok_or_else(|| "SDCP v1 requires an IPv4 printer address".to_string())
}

fn parse_discovery(bytes: &[u8]) -> Result<DiscoveryResponse, String> {
    let response: Value =
        serde_json::from_slice(bytes).map_err(|error| format!("Invalid SDCP discovery response: {error}"))?;
    let data = response.get("Data").cloned().unwrap_or(Value::Null);
    let attributes = data.get("Attributes").cloned().unwrap_or(Value::Null);
    let mainboard_id = attributes
        .get("MainboardID")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if mainboard_id.is_empty()
        || !mainboard_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return Err("SDCP discovery response did not contain MainboardID".to_string());
    }
    let id = response.get("Id").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if id.is_empty() {
        return Err("SDCP discovery response did not contain Id".to_string());
    }

    Ok(DiscoveryResponse {
        id,
        mainboard_id,
        data,
    })
}

fn discovery_request(host: Ipv4Addr) -> Result<(DiscoveryResponse, Ipv4Addr), String> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|error| format!("Could not open SDCP v1 UDP socket: {error}"))?;
    socket
        .set_read_timeout(Some(DEFAULT_TIMEOUT))
        .map_err(|error| format!("Could not configure SDCP v1 UDP socket: {error}"))?;
    socket
        .send_to(b"M99999", SocketAddrV4::new(host, DISCOVERY_PORT))
        .map_err(|error| format!("Could not query SDCP v1 printer: {error}"))?;
    let mut buffer = [0u8; 16 * 1024];
    let (size, sender) = socket
        .recv_from(&mut buffer)
        .map_err(|error| format!("No SDCP v1 discovery response from {host}: {error}"))?;
    let response = parse_discovery(&buffer[..size])?;
    let sender_ip = match sender.ip() {
        IpAddr::V4(address) => address,
        IpAddr::V6(_) => return Err("SDCP v1 requires an IPv4 printer address".to_string()),
    };
    Ok((response, sender_ip))
}

fn discovery_status(response: &DiscoveryResponse, host: Ipv4Addr) -> Value {
    let attributes = response.data.get("Attributes").cloned().unwrap_or(Value::Null);
    let status = response.data.get("Status").cloned().unwrap_or(Value::Null);
    let print_info = status.get("PrintInfo").cloned().unwrap_or(Value::Null);
    let current_status = status
        .get("CurrentStatus")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let print_status = print_info.get("Status").and_then(Value::as_i64).unwrap_or(0);
    let current_layer = print_info.get("CurrentLayer").and_then(Value::as_i64);
    let total_layers = print_info
        .get("TotalLayer")
        .or_else(|| print_info.get("TotalLayers"))
        .and_then(Value::as_i64);
    let paused = print_status == 7;
    let finished = print_status == 16 || print_status == 9;
    let printing = current_status == 1 && !paused && !finished;
    let progress = match (current_layer, total_layers) {
        (Some(current), Some(total)) if total > 0 => {
            Some(((current as f64 / total as f64) * 100.0).clamp(0.0, 100.0))
        }
        _ => None,
    };

    let host_string = host.to_string();
    json!({
        "ok": true,
        "connected": true,
        "mode": "sdcp-v1",
        "hostName": attributes.get("Name").and_then(Value::as_str).unwrap_or(&host_string),
        "printerName": attributes.get("MachineName").and_then(Value::as_str).unwrap_or("SDCP v1 Printer"),
        "printerModel": attributes.get("MachineName").and_then(Value::as_str).unwrap_or("SDCP v1"),
        "ipAddress": host.to_string(),
        "port": DISCOVERY_PORT,
        "firmwareVersion": attributes.get("FirmwareVersion").and_then(Value::as_str).unwrap_or(""),
        "mainboardId": response.mainboard_id,
        "currentStatus": current_status,
        "printStatus": print_status,
        "status": status,
        "stateText": if paused { "Paused" } else if finished { "Finished" } else if printing { "Printing" } else { "Online" },
        "state": if paused { "paused" } else if finished { "finished" } else if printing { "printing" } else { "idle" },
        "isPrinting": printing,
        "isPaused": paused,
        "finished": finished,
        "progressPct": progress,
        "currentLayer": current_layer,
        "totalLayers": total_layers,
        "jobName": print_info.get("Filename").and_then(Value::as_str),
        "etaSec": Value::Null,
        "raw": response.data,
    })
}

fn encode_remaining_length(mut length: usize) -> Vec<u8> {
    let mut encoded = Vec::new();
    loop {
        let mut byte = (length % 128) as u8;
        length /= 128;
        if length > 0 {
            byte |= 0x80;
        }
        encoded.push(byte);
        if length == 0 {
            return encoded;
        }
    }
}

fn write_mqtt_packet(stream: &mut TcpStream, header: u8, body: &[u8]) -> Result<(), String> {
    stream
        .write_all(&[header])
        .and_then(|_| stream.write_all(&encode_remaining_length(body.len())))
        .and_then(|_| stream.write_all(body))
        .map_err(|error| format!("SDCP v1 MQTT write failed: {error}"))
}

fn read_mqtt_packet(stream: &mut TcpStream) -> Result<(u8, Vec<u8>), String> {
    let mut header = [0u8; 1];
    stream
        .read_exact(&mut header)
        .map_err(|error| format!("SDCP v1 MQTT connection closed: {error}"))?;
    let mut remaining = 0usize;
    let mut multiplier = 1usize;
    let mut byte = 0u8;
    for _ in 0..4 {
        stream
            .read_exact(std::slice::from_mut(&mut byte))
            .map_err(|error| format!("Malformed SDCP v1 MQTT packet: {error}"))?;
        remaining += usize::from(byte & 0x7f) * multiplier;
        if byte & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    if remaining > MAX_MQTT_PACKET_BYTES {
        return Err("SDCP v1 MQTT packet exceeds the size limit".to_string());
    }
    let mut body = vec![0u8; remaining];
    stream
        .read_exact(&mut body)
        .map_err(|error| format!("Incomplete SDCP v1 MQTT packet: {error}"))?;
    Ok((header[0], body))
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(offset)?, *bytes.get(offset + 1)?]))
}

fn mqtt_session(host: Ipv4Addr) -> Result<MqttSession, String> {
    let udp = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|error| format!("Could not open SDCP v1 UDP socket: {error}"))?;
    udp.connect(SocketAddrV4::new(host, DISCOVERY_PORT))
        .map_err(|error| format!("Could not route to SDCP v1 printer: {error}"))?;
    udp.set_read_timeout(Some(DEFAULT_TIMEOUT))
        .map_err(|error| format!("Could not configure SDCP v1 UDP socket: {error}"))?;
    udp.send(b"M99999")
        .map_err(|error| format!("Could not query the SDCP v1 printer: {error}"))?;
    let mut discovery_buffer = [0u8; 16 * 1024];
    let discovery_size = udp
        .recv(&mut discovery_buffer)
        .map_err(|error| format!("No SDCP v1 discovery response from {host}: {error}"))?;
    let discovery = parse_discovery(&discovery_buffer[..discovery_size])?;
    let local_ip = match udp.local_addr().map_err(|error| error.to_string())?.ip() {
        std::net::IpAddr::V4(address) => address,
        _ => return Err("SDCP v1 requires an IPv4 network interface".to_string()),
    };
    let listener = TcpListener::bind(SocketAddrV4::new(local_ip, 0))
        .map_err(|error| format!("Could not start SDCP v1 MQTT broker: {error}"))?;
    listener
        .set_nonblocking(false)
        .map_err(|error| format!("Could not configure SDCP v1 MQTT broker: {error}"))?;
    let mqtt_port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    udp.send(format!("M66666 {mqtt_port}").as_bytes())
        .map_err(|error| format!("Could not request an SDCP v1 MQTT connection: {error}"))?;

    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("Could not configure SDCP v1 MQTT broker: {error}"))?;
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("Timed out waiting for the SDCP v1 printer MQTT connection".to_string());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(format!("Could not accept the SDCP v1 MQTT connection: {error}")),
        }
    };
    let _ = stream.set_nonblocking(false);
    stream
        .set_read_timeout(Some(DEFAULT_TIMEOUT))
        .map_err(|error| format!("Could not configure SDCP v1 MQTT connection: {error}"))?;
    stream
        .set_write_timeout(Some(DEFAULT_TIMEOUT))
        .map_err(|error| format!("Could not configure SDCP v1 MQTT connection: {error}"))?;

    let (header, _) = read_mqtt_packet(&mut stream)?;
    if header >> 4 != 1 {
        return Err("Expected MQTT CONNECT from the SDCP v1 printer".to_string());
    }
    write_mqtt_packet(&mut stream, 0x20, &[0, 0])?;
    loop {
        let (header, body) = read_mqtt_packet(&mut stream)?;
        match header >> 4 {
            8 if body.len() >= 4 => {
                let packet_id = read_u16(&body, 0)
                    .ok_or_else(|| "Invalid MQTT SUBSCRIBE packet".to_string())?;
                write_mqtt_packet(
                    &mut stream,
                    0x90,
                    &[packet_id.to_be_bytes()[0], packet_id.to_be_bytes()[1], 0],
                )?;
                break;
            }
            12 => write_mqtt_packet(&mut stream, 0xd0, &[])?,
            6 if body.len() >= 2 => write_mqtt_packet(&mut stream, 0x70, &body[..2])?,
            3 => acknowledge_publish(&mut stream, header, &body)?,
            _ => {}
        }
    }

    let mut session = MqttSession {
        stream,
        mainboard_id: discovery.mainboard_id,
        connection_id: discovery.id,
    };
    session.request(0, Value::Null)?;
    session.request(1, Value::Null)?;
    session.request(512, json!({ "TimePeriod": 5000 }))?;
    Ok(session)
}

fn mqtt_publish_packet(topic: &str, payload: &[u8]) -> Result<Vec<u8>, String> {
    let topic_len = u16::try_from(topic.len())
        .map_err(|_| "SDCP v1 MQTT topic is too long".to_string())?;
    let mut body = Vec::with_capacity(topic.len() + payload.len() + 2);
    body.extend_from_slice(&topic_len.to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    body.extend_from_slice(payload);
    let mut packet = vec![0x30];
    packet.extend(encode_remaining_length(body.len()));
    packet.extend(body);
    Ok(packet)
}

fn publish_packet_id(header: u8, body: &[u8]) -> Option<u16> {
    let topic_len = usize::from(read_u16(body, 0)?);
    if body.len() < 2 + topic_len {
        return None;
    }
    let qos = (header >> 1) & 0x03;
    if qos == 0 {
        return None;
    }
    read_u16(body, 2 + topic_len)
}

fn acknowledge_publish(stream: &mut TcpStream, header: u8, body: &[u8]) -> Result<(), String> {
    let qos = (header >> 1) & 0x03;
    if qos == 1 {
        if let Some(packet_id) = publish_packet_id(header, body) {
            write_mqtt_packet(stream, 0x40, &packet_id.to_be_bytes())?;
        }
    } else if qos == 2 {
        if let Some(packet_id) = publish_packet_id(header, body) {
            write_mqtt_packet(stream, 0x50, &packet_id.to_be_bytes())?;
        }
    }
    Ok(())
}

impl MqttSession {
    fn request(&mut self, command: u16, data: Value) -> Result<Value, String> {
        let id = request_id();
        let topic = format!("/sdcp/request/{}", self.mainboard_id);
        let payload = json!({
            "Id": self.connection_id,
            "Data": {
                "Cmd": command,
                "Data": data,
                "From": 0,
                "MainboardID": self.mainboard_id,
                "RequestID": id,
                "TimeStamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            },
        });
        let bytes = serde_json::to_vec(&payload).map_err(|error| error.to_string())?;
        let packet = mqtt_publish_packet(&topic, &bytes)?;
        self.stream
            .write_all(&packet)
            .map_err(|error| format!("Could not send SDCP v1 command {command}: {error}"))?;

        let response_topic = format!("/sdcp/response/{}", self.mainboard_id);
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err(format!("Timed out waiting for SDCP v1 command {command}"));
            }
            let (header, body) = read_mqtt_packet(&mut self.stream)?;
            match header >> 4 {
                3 => {
                    acknowledge_publish(&mut self.stream, header, &body)?;
                    if let Some((topic, value)) = decode_mqtt_publish(header, &body) {
                        if topic == response_topic
                            && value
                                .pointer("/Data/RequestID")
                                .and_then(Value::as_str)
                                == Some(id.as_str())
                        {
                            let ack = value
                                .pointer("/Data/Data/Ack")
                                .and_then(Value::as_i64)
                                .unwrap_or(0);
                            if ack != 0 {
                                return Err(format!("SDCP v1 command {command} was rejected (Ack {ack})"));
                            }
                            return Ok(value);
                        }
                    }
                }
                12 => write_mqtt_packet(&mut self.stream, 0xd0, &[])?,
                6 if body.len() >= 2 => write_mqtt_packet(&mut self.stream, 0x70, &body[..2])?,
                _ => {}
            }
        }
    }
}

fn decode_mqtt_publish(header: u8, body: &[u8]) -> Option<(String, Value)> {
    let topic_len = usize::from(read_u16(body, 0)?);
    if body.len() < 2 + topic_len {
        return None;
    }
    let topic = String::from_utf8(body[2..2 + topic_len].to_vec()).ok()?;
    let payload_start = 2 + topic_len + if ((header >> 1) & 0x03) == 0 { 0 } else { 2 };
    if body.len() < payload_start {
        return None;
    }
    let payload = serde_json::from_slice(&body[payload_start..]).ok()?;
    Some((topic, payload))
}

fn send_status(host: Ipv4Addr) -> Result<Value, String> {
    let (discovery, sender) = discovery_request(host)?;
    Ok(discovery_status(&discovery, sender))
}

fn discover(timeout_ms: u64) -> Result<Vec<Value>, String> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|error| format!("Could not open SDCP v1 discovery socket: {error}"))?;
    socket
        .set_broadcast(true)
        .map_err(|error| format!("Could not enable SDCP v1 broadcast: {error}"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(120)))
        .map_err(|error| format!("Could not configure SDCP v1 discovery socket: {error}"))?;

    let mut targets = vec![Ipv4Addr::BROADCAST];
    if let Ok(interfaces) = if_addrs::get_if_addrs() {
        for interface in interfaces {
            if interface.is_loopback() {
                continue;
            }
            if let if_addrs::IfAddr::V4(v4) = interface.addr {
                let ip = u32::from(v4.ip);
                let mask = u32::from(v4.netmask);
                targets.push(Ipv4Addr::from((ip & mask) | !mask));
            }
        }
    }
    targets.sort();
    targets.dedup();
    for target in targets {
        let _ = socket.send_to(b"M99999", SocketAddrV4::new(target, DISCOVERY_PORT));
    }

    let deadline = Instant::now() + Duration::from_millis(timeout_ms.clamp(300, 8000));
    let mut found = HashMap::new();
    let mut buffer = [0u8; 16 * 1024];
    while Instant::now() < deadline {
        if let Ok((size, sender)) = socket.recv_from(&mut buffer) {
            let IpAddr::V4(address) = sender.ip() else {
                continue;
            };
            if let Ok(response) = parse_discovery(&buffer[..size]) {
                found.insert(
                    address.to_string(),
                    discovery_status(&response, address),
                );
            }
        }
    }
    let devices: Vec<Value> = found.into_values().collect();
    Ok(devices)
}

use std::net::IpAddr;

fn upload_dir() -> Result<PathBuf, String> {
    let directory = std::env::temp_dir().join("dragonfruit-sdcp-v1");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create SDCP v1 upload directory: {error}"))?;
    Ok(directory)
}

fn safe_upload_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 80 && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn upload_chunk(payload: &Value) -> Result<Value, String> {
    let id = payload_string(payload, &["uuid", "uploadId"])
        .ok_or_else(|| "An upload ID is required".to_string())?;
    if !safe_upload_id(id) {
        return Err("Invalid upload ID".to_string());
    }
    let file_name = payload_string(payload, &["fileName"])
        .ok_or_else(|| "A file name is required".to_string())?;
    let file_name = PathBuf::from(file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.to_ascii_lowercase().ends_with(".ctb") || name.to_ascii_lowercase().ends_with(".goo"))
        .ok_or_else(|| "SDCP v1 upload supports .ctb and .goo files".to_string())?
        .to_string();
    let total_size = payload
        .get("totalSize")
        .and_then(Value::as_u64)
        .filter(|size| *size > 0 && *size <= 1024 * 1024 * 1024)
        .ok_or_else(|| "Invalid upload size".to_string())?;
    let offset = payload
        .get("offset")
        .and_then(Value::as_u64)
        .ok_or_else(|| "Invalid upload offset".to_string())?;
    let chunk = payload_string(payload, &["chunkBase64"])
        .ok_or_else(|| "Upload chunk data is required".to_string())?;
    if chunk.len() > 2 * 1024 * 1024 {
        return Err("SDCP v1 upload chunks may not exceed 1 MiB".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(chunk)
        .map_err(|error| format!("Invalid upload chunk encoding: {error}"))?;
    if bytes.len() > 1024 * 1024 {
        return Err("SDCP v1 upload chunks may not exceed 1 MiB".to_string());
    }

    let mut map = uploads()
        .lock()
        .map_err(|_| "SDCP v1 upload registry is unavailable".to_string())?;
    if !map.contains_key(id) {
        if offset != 0 {
            return Err("The first SDCP v1 upload chunk must start at offset zero".to_string());
        }
        let path = upload_dir()?.join(format!("{id}.part"));
        map.insert(
            id.to_string(),
            UploadEntry {
                file_path: path,
                file_name: file_name.clone(),
                total_size,
                written: 0,
            },
        );
    }
    let entry = map
        .get_mut(id)
        .ok_or_else(|| "SDCP v1 upload state was lost".to_string())?;
    if entry.file_name != file_name || entry.total_size != total_size || entry.written != offset {
        return Err("SDCP v1 upload chunk does not match the active upload".to_string());
    }
    if bytes.is_empty() || entry.written + bytes.len() as u64 > total_size {
        return Err("Invalid SDCP v1 upload chunk size".to_string());
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&entry.file_path)
        .map_err(|error| format!("Could not write SDCP v1 upload chunk: {error}"))?;
    file.write_all(&bytes)
        .map_err(|error| format!("Could not write SDCP v1 upload chunk: {error}"))?;
    entry.written += bytes.len() as u64;
    Ok(json!({ "ok": true, "uploaded": entry.written, "totalSize": total_size }))
}

fn file_md5(path: &PathBuf) -> Result<String, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("Could not read uploaded SDCP v1 file: {error}"))?;
    let mut digest = Md5::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let size = file
            .read(&mut buffer)
            .map_err(|error| format!("Could not hash uploaded SDCP v1 file: {error}"))?;
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn serve_file(
    listener: TcpListener,
    path: PathBuf,
    expected_name: String,
    digest: String,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("Could not configure SDCP v1 file server: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if cancelled.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err("Timed out waiting for the SDCP v1 printer to request the file".to_string());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(format!("SDCP v1 printer could not download the print file: {error}")),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(180)))
        .map_err(|error| error.to_string())?;
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while request.len() < 8192 && !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let size = stream
            .read(&mut chunk)
            .map_err(|error| format!("Invalid SDCP v1 HTTP request: {error}"))?;
        if size == 0 {
            return Err("SDCP v1 printer closed its file request early".to_string());
        }
        request.extend_from_slice(&chunk[..size]);
    }
    let request_text = String::from_utf8_lossy(&request);
    if !request.windows(4).any(|window| window == b"\r\n\r\n") {
        return Err("SDCP v1 printer sent an invalid HTTP request".to_string());
    }
    let request_line = request_text.lines().next().unwrap_or("");
    let expected_path = format!("/{expected_name}");
    if request_line.starts_with(&format!("HEAD {expected_path} ")) {
        let size = fs::metadata(&path)
            .map_err(|error| format!("Could not read SDCP v1 print file: {error}"))?
            .len();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nETag: {digest}\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
        )
        .map_err(|error| format!("Could not respond to SDCP v1 file probe: {error}"))?;
        return serve_file(listener, path, expected_name, digest, cancelled);
    }
    if !request_line.starts_with(&format!("GET {expected_path} ")) {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return Err("SDCP v1 printer requested an unexpected file path".to_string());
    }
    let mut file = File::open(&path)
        .map_err(|error| format!("Could not open SDCP v1 print file: {error}"))?;
    let size = file
        .metadata()
        .map_err(|error| format!("Could not read SDCP v1 print file: {error}"))?
        .len();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nETag: {digest}\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
    )
    .and_then(|_| std::io::copy(&mut file, &mut stream).map(|_| ()))
    .map_err(|error| format!("Could not serve SDCP v1 print file: {error}"))
}

fn upload_file(host: Ipv4Addr, payload: &Value) -> Result<Value, String> {
    let id = payload_string(payload, &["uuid", "uploadId"])
        .ok_or_else(|| "An upload ID is required".to_string())?;
    let entry = {
        let map = uploads()
            .lock()
            .map_err(|_| "SDCP v1 upload registry is unavailable".to_string())?;
        let entry = map
            .get(id)
            .ok_or_else(|| "SDCP v1 upload was not found".to_string())?;
        if entry.written != entry.total_size {
            return Err("SDCP v1 upload is incomplete".to_string());
        }
        (entry.file_path.clone(), entry.file_name.clone(), entry.total_size)
    };
    let digest = file_md5(&entry.0)?;
    let http_listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|error| format!("Could not start SDCP v1 file server: {error}"))?;
    let http_port = http_listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let http_name = format!("{id}.{}", entry.1.rsplit('.').next().unwrap_or("ctb"));
    let mut session = mqtt_session(host)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let server_path = entry.0.clone();
    let server_name = http_name.clone();
    let server_digest = digest.clone();
    let server_cancelled = Arc::clone(&cancelled);
    let server = std::thread::spawn(move || {
        let result = serve_file(
            http_listener,
            server_path,
            server_name,
            server_digest,
            &server_cancelled,
        );
        if let Err(error) = &result {
            log::error!("[sdcp-v1] HTTP file server failed: {error}");
        }
        result
    });
    let command = json!({
        "Check": 0,
        "CleanCache": 1,
        "Compress": 0,
        "FileSize": entry.2,
        "Filename": entry.1,
        "MD5": digest,
        "URL": format!("http://${{ipaddr}}:{http_port}/{http_name}"),
    });
    if let Err(error) = session.request(256, command) {
        cancelled.store(true, Ordering::Relaxed);
        let _ = server.join();
        return Err(error);
    }

    let transfer_result = (|| {
        let deadline = Instant::now() + Duration::from_secs(180);
        while Instant::now() < deadline {
            let (header, body) = read_mqtt_packet(&mut session.stream)?;
            if header >> 4 == 12 {
                write_mqtt_packet(&mut session.stream, 0xd0, &[])?;
                continue;
            }
            if header >> 4 == 6 && body.len() >= 2 {
                write_mqtt_packet(&mut session.stream, 0x70, &body[..2])?;
                continue;
            }
            if header >> 4 != 3 {
                continue;
            }
            acknowledge_publish(&mut session.stream, header, &body)?;
            let Some((topic, status)) = decode_mqtt_publish(header, &body) else {
                continue;
            };
            if topic != format!("/sdcp/status/{}", session.mainboard_id) {
                continue;
            }
            if let Some(result) = file_transfer_completion(&status) {
                return result;
            }
        }
        Err("Timed out waiting for the SDCP v1 printer to finish downloading the file".to_string())
    })();
    cancelled.store(true, Ordering::Relaxed);
    let server_result = server.join().map_err(|_| "SDCP v1 file server stopped unexpectedly".to_string())?;
    transfer_result?;
    server_result?;

    let plate_id = stable_plate_id(&entry.1);
    if let Ok(mut files) = print_files().lock() {
        files.insert(format!("{}:{plate_id}", host), entry.1);
    }
    if let Ok(mut map) = uploads().lock() {
        map.remove(id);
    }
    let _ = fs::remove_file(entry.0);
    Ok(json!({ "ok": true, "plateId": plate_id }))
}

fn stable_plate_id(file_name: &str) -> i64 {
    let mut value = 2166136261u32;
    for byte in file_name.bytes() {
        value ^= u32::from(byte);
        value = value.wrapping_mul(16777619);
    }
    i64::from((value & 0x7fff_ffff).max(1))
}

fn file_transfer_completion(status: &Value) -> Option<Result<(), String>> {
    let status_data = status.pointer("/Data/Status")?;
    if status_data.get("CurrentStatus").and_then(Value::as_i64) != Some(0) {
        return None;
    }
    let file_transfer = status_data.get("FileTransferInfo")?;
    let transfer_status = file_transfer.get("Status").and_then(Value::as_i64).unwrap_or(0);
    let downloaded = file_transfer
        .get("DownloadOffset")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = file_transfer
        .get("FileTotalSize")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    match transfer_status {
        2 => Some(Ok(())),
        3 => Some(Err(format!(
            "The SDCP v1 printer reported a file transfer error after downloading {downloaded} of {total} bytes"
        ))),
        status => Some(Err(format!(
            "The SDCP v1 printer returned to ready state with unexpected file transfer status {status} ({downloaded} of {total} bytes)"
        ))),
    }
}

fn control(host: Ipv4Addr, command: u16, payload: &Value) -> Result<Value, String> {
    let data = match command {
        128 => {
            let plate_id = payload.get("plateId").and_then(Value::as_i64).unwrap_or(0);
            let file_name = print_files()
                .lock()
                .map_err(|_| "SDCP v1 print-file registry is unavailable".to_string())?
                .get(&format!("{}:{plate_id}", host))
                .cloned()
                .or_else(|| {
                    payload_string(payload, &["fileName", "filename"]).map(str::to_string)
                })
                .ok_or_else(|| "The SDCP v1 print file is unknown; upload it before starting".to_string())?;
            json!({ "Filename": file_name, "StartLayer": 0 })
        }
        _ => Value::Null,
    };
    mqtt_session(host)?.request(command, data)?;
    Ok(json!({ "ok": true }))
}

fn dispatch(operation: &str, payload: &Value) -> (u16, Value) {
    let normalized = operation.trim().trim_start_matches('/');
    let op = normalized.strip_prefix("sdcp/").unwrap_or(normalized);
    match op {
        "discover" => {
            let timeout = payload
                .get("probeTimeoutMs")
                .and_then(Value::as_u64)
                .unwrap_or(1200)
                .clamp(300, 8000);
            match discover(timeout) {
                Ok(devices) => (
                    200,
                    json!({
                        "mode": "sdcp-v1",
                        "devices": devices,
                        "scannedHosts": devices.len(),
                        "scannedEndpoints": devices.len(),
                        "scanScope": payload.get("scanScope").and_then(Value::as_str).unwrap_or("all"),
                        "progressive": false,
                        "totalEndpoints": devices.len(),
                        "batchStart": 0,
                        "batchSize": devices.len(),
                        "nextBatchStart": devices.len(),
                        "done": true,
                    }),
                ),
                Err(error) => (502, json!({ "ok": false, "error": error })),
            }
        }
        "connect" | "printer/status" => {
            let host = payload_string(payload, &["host", "ipAddress"])
                .ok_or_else(|| "Printer host is required".to_string())
                .and_then(parse_ipv4_host);
            match host.and_then(|host| {
                if op == "connect" {
                    mqtt_session(host)?;
                }
                send_status(host)
            }) {
                Ok(status) => {
                    if op == "connect" {
                        (200, status)
                    } else {
                        (200, status)
                    }
                }
                Err(error) => (
                    503,
                    json!({
                        "ok": false,
                        "connected": false,
                        "mode": "sdcp-v1",
                        "stateText": "Offline",
                        "error": error,
                    }),
                ),
            }
        }
        "printer/webcam/info" => (
            200,
            json!({
                "available": false,
                "streamUrl": Value::Null,
                "snapshotUrl": Value::Null,
                "message": "SDCP v1 does not provide webcam endpoints.",
            }),
        ),
        "plates/list/json" => (
            200,
            json!({
                "ok": true,
                "plates": [],
                "metadataReady": true,
            }),
        ),
        "materials" | "materials/edit" | "unsupported" => {
            (404, json!({ "error": "Remote material profiles are not supported by SDCP v1." }))
        }
        "upload/chunk" => match upload_chunk(payload) {
            Ok(result) => (200, result),
            Err(error) => (400, json!({ "ok": false, "error": error })),
        },
        "upload/finish" => {
            let host = payload_string(payload, &["host", "ipAddress"])
                .ok_or_else(|| "Printer host is required".to_string())
                .and_then(parse_ipv4_host);
            match host.and_then(|host| upload_file(host, payload)) {
                Ok(result) => (200, result),
                Err(error) => (502, json!({ "ok": false, "error": error })),
            }
        }
        "printer/start" | "printer/pause" | "printer/resume" | "printer/unpause"
        | "printer/stop" | "printer/cancel" | "printer/force-stop" => {
            let host = payload_string(payload, &["host", "ipAddress"])
                .ok_or_else(|| "Printer host is required".to_string())
                .and_then(parse_ipv4_host);
            let command = match op {
                "printer/start" => 128,
                "printer/pause" => 129,
                "printer/resume" | "printer/unpause" => 131,
                _ => 130,
            };
            match host.and_then(|host| control(host, command, payload)) {
                Ok(result) => (200, result),
                Err(error) => (502, json!({ "ok": false, "error": error })),
            }
        }
        "plate/delete" => (501, json!({ "ok": false, "error": "SDCP v1 does not support deleting plates." })),
        _ => (404, json!({ "error": format!("Unknown SDCP v1 operation: {op}") })),
    }
}

pub async fn dispatch_plugin_network_request(request_json: String) -> Result<PluginNetworkResponse, String> {
    let request: Value =
        serde_json::from_str(&request_json).map_err(|error| format!("Invalid request JSON: {error}"))?;
    let plugin_id = request
        .get("pluginId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let operation = request
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if plugin_id != "sdcp-v1" {
        return Ok(PluginNetworkResponse {
            status: 404,
            body: json!({ "error": format!("Unknown network plugin: {plugin_id}") }),
        });
    }
    if operation.is_empty() {
        return Ok(PluginNetworkResponse {
            status: 400,
            body: json!({ "error": "operation is required" }),
        });
    }
    let log_operation = operation.clone();
    match tokio::task::spawn_blocking(move || dispatch(&operation, &request)).await {
        Ok((status, body)) => {
            if status >= 400 {
                let error = body
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown SDCP v1 network error");
                log::error!(
                    "[sdcp-v1] Operation {log_operation} failed (HTTP {status}): {error}"
                );
            }
            Ok(PluginNetworkResponse { status, body })
        }
        Err(error) => {
            log::error!("[sdcp-v1] Operation {log_operation} task failed: {error}");
            Err(format!("SDCP v1 network operation failed: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mqtt_publish_packets_keep_qos_zero_payload_intact() {
        let payload = br#"{"Data":{"RequestID":"sample"}}"#;
        let packet = mqtt_publish_packet("/sdcp/response/board", payload).unwrap();

        let topic = "/sdcp/response/board";
        let mut body = Vec::new();
        body.extend_from_slice(&(topic.len() as u16).to_be_bytes());
        body.extend_from_slice(topic.as_bytes());
        body.extend_from_slice(payload);
        let decoded = decode_mqtt_publish(0x30, &body).unwrap();
        assert_eq!(decoded.0, topic);
        assert_eq!(decoded.1["Data"]["RequestID"], "sample");
        assert!(packet.starts_with(&[0x30]));
    }

    #[test]
    fn parses_saturn_style_discovery_status() {
        let response = parse_discovery(
            br#"{
                "Id":"session-id",
                "Data":{
                    "Attributes":{"MainboardID":"board-1","MachineName":"ELEGOO Saturn 3 Ultra"},
                    "Status":{"CurrentStatus":1,"PrintInfo":{"Status":3,"CurrentLayer":25,"TotalLayer":100,"Filename":"part.ctb"}}
                }
            }"#,
        )
        .unwrap();
        let status = discovery_status(&response, Ipv4Addr::new(192, 168, 1, 25));
        assert_eq!(status["mainboardId"], "board-1");
        assert_eq!(status["progressPct"], 25.0);
        assert_eq!(status["stateText"], "Printing");
        assert_eq!(status["jobName"], "part.ctb");
    }

    #[test]
    fn waits_for_printer_ready_before_treating_transfer_status_as_final() {
        let stale_error = json!({
            "Data": {
                "Status": {
                    "CurrentStatus": 1,
                    "FileTransferInfo": {
                        "Status": 3,
                        "DownloadOffset": 0,
                        "FileTotalSize": 100,
                    },
                },
            },
        });
        assert!(file_transfer_completion(&stale_error).is_none());
    }

    #[test]
    fn reports_final_transfer_status_with_download_progress() {
        let failed = json!({
            "Data": {
                "Status": {
                    "CurrentStatus": 0,
                    "FileTransferInfo": {
                        "Status": 3,
                        "DownloadOffset": 25,
                        "FileTotalSize": 100,
                    },
                },
            },
        });
        assert_eq!(
            file_transfer_completion(&failed).unwrap().unwrap_err(),
            "The SDCP v1 printer reported a file transfer error after downloading 25 of 100 bytes"
        );
    }

    #[test]
    fn reports_metadata_ready_for_v1_after_upload_success() {
        let (_, response) = dispatch("plates/list/json", &json!({}));
        assert_eq!(response["ok"], true);
        assert_eq!(response["plates"], json!([]));
        assert_eq!(response["metadataReady"], true);
    }
}
