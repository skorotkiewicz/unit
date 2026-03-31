// ws_bridge.rs — WebSocket bridge for browser-to-mesh communication
//
// A raw TCP WebSocket server (no external deps) that bridges WASM browser
// units to the native UDP mesh. Implements the WebSocket handshake (RFC 6455),
// frame encode/decode with masking, and a JSON message protocol.
//
// Architecture: native unit ↔ WS bridge ↔ browser WASM unit
//               native unit ↔ UDP gossip ↔ other native units

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// SHA-1 (minimal, for WebSocket handshake only — not for security)
// ---------------------------------------------------------------------------

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x67452301;
    let mut h1: u32 = 0xEFCDAB89;
    let mut h2: u32 = 0x98BADCFE;
    let mut h3: u32 = 0x10325476;
    let mut h4: u32 = 0xC3D2E1F0;

    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (i, &w_val) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w_val);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

// ---------------------------------------------------------------------------
// WebSocket frame encode/decode (RFC 6455)
// ---------------------------------------------------------------------------

fn ws_accept_key(client_key: &str) -> String {
    let magic = "258EAFA5-E914-47DA-95CA-5AB5DC799B07";
    let combined = format!("{}{}", client_key.trim(), magic);
    let hash = sha1(combined.as_bytes());
    base64_encode(&hash)
}

fn ws_encode_text(payload: &str) -> Vec<u8> {
    let data = payload.as_bytes();
    let mut frame = Vec::with_capacity(10 + data.len());
    frame.push(0x81); // FIN + text opcode
    if data.len() < 126 {
        frame.push(data.len() as u8);
    } else if data.len() < 65536 {
        frame.push(126);
        frame.extend_from_slice(&(data.len() as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(data.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(data);
    frame
}

fn ws_decode_frame(data: &[u8]) -> Option<(String, usize)> {
    if data.len() < 2 {
        return None;
    }
    let _opcode = data[0] & 0x0F;
    let masked = (data[1] & 0x80) != 0;
    let mut payload_len = (data[1] & 0x7F) as usize;
    let mut offset = 2;

    if payload_len == 126 {
        if data.len() < 4 {
            return None;
        }
        payload_len = u16::from_be_bytes([data[2], data[3]]) as usize;
        offset = 4;
    } else if payload_len == 127 {
        if data.len() < 10 {
            return None;
        }
        payload_len = u64::from_be_bytes(data[2..10].try_into().ok()?) as usize;
        offset = 10;
    }

    let mask_key = if masked {
        if data.len() < offset + 4 {
            return None;
        }
        let key = [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ];
        offset += 4;
        Some(key)
    } else {
        None
    };

    if data.len() < offset + payload_len {
        return None;
    }

    let mut payload = data[offset..offset + payload_len].to_vec();
    if let Some(key) = mask_key {
        for i in 0..payload.len() {
            payload[i] ^= key[i % 4];
        }
    }

    let text = String::from_utf8_lossy(&payload).to_string();
    Some((text, offset + payload_len))
}

fn ws_encode_pong(ping_data: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(2 + ping_data.len());
    frame.push(0x8A); // FIN + pong opcode
    frame.push(ping_data.len() as u8);
    frame.extend_from_slice(ping_data);
    frame
}

// ---------------------------------------------------------------------------
// Client tracking
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct WsClient {
    pub id: String,
    pub connected_at: Instant,
    pub last_heartbeat: Instant,
    pub fitness: i64,
}

pub struct WsBridgeState {
    pub clients: HashMap<String, WsClient>,
    pub messages_relayed: u64,
    pub port: u16,
}

impl WsBridgeState {
    pub fn new(port: u16) -> Self {
        WsBridgeState {
            clients: HashMap::new(),
            messages_relayed: 0,
            port,
        }
    }

    pub fn format_status(&self) -> String {
        format!(
            "ws-bridge: port={} browsers={} relayed={}\n",
            self.port,
            self.clients.len(),
            self.messages_relayed
        )
    }

    pub fn format_clients(&self) -> String {
        if self.clients.is_empty() {
            return "  (no browsers connected)\n".to_string();
        }
        let mut out = String::new();
        for (id, c) in &self.clients {
            let age = c.connected_at.elapsed().as_secs();
            out.push_str(&format!(
                "  {} fitness={} connected={}s\n",
                id, c.fitness, age
            ));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// WebSocket server
// ---------------------------------------------------------------------------

/// Messages from the WS bridge to the main VM thread.
#[derive(Clone, Debug)]
pub enum WsEvent {
    /// A browser client sent a goal submission.
    GoalSubmit { code: String, priority: i64 },
    /// A browser client connected.
    ClientConnected { id: String },
    /// A browser client disconnected.
    ClientDisconnected { id: String },
    /// Browser heartbeat with fitness.
    Heartbeat { id: String, fitness: i64 },
}

/// Start the WebSocket bridge server. Returns a shared state handle and
/// an event receiver for the VM to poll.
pub fn start_ws_bridge(
    port: u16,
    mesh_state_json: Arc<Mutex<String>>, // periodically updated by VM
) -> Result<
    (
        Arc<Mutex<WsBridgeState>>,
        std::sync::mpsc::Receiver<WsEvent>,
    ),
    String,
> {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))
        .map_err(|e| format!("ws bind {}: {}", port, e))?;
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("ws nonblock: {}", e))?;

    let state = Arc::new(Mutex::new(WsBridgeState::new(port)));
    let (tx, rx) = std::sync::mpsc::channel();

    let state_clone = state.clone();
    let mesh_json = mesh_state_json.clone();

    std::thread::spawn(move || {
        // Accept connections sequentially (simple for the seed).
        // Each connection is handled in its own thread.
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            let state = state_clone.clone();
            let mesh_json = mesh_json.clone();
            std::thread::spawn(move || {
                handle_ws_client(stream, tx, state, mesh_json);
            });
        }
    });

    Ok((state, rx))
}

fn handle_ws_client(
    mut stream: TcpStream,
    tx: std::sync::mpsc::Sender<WsEvent>,
    state: Arc<Mutex<WsBridgeState>>,
    mesh_json: Arc<Mutex<String>>,
) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    // Read the FULL HTTP request (all headers up to \r\n\r\n).
    // TCP may fragment the data, so we loop until we see the header terminator.
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => return,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                // Check if we have the complete headers.
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 8192 {
                    return;
                } // safety limit
            }
            Err(_) => return,
        }
    }
    let request = String::from_utf8_lossy(&buf).to_string();
    let is_upgrade = request
        .lines()
        .any(|l| l.to_lowercase().contains("upgrade: websocket"));

    // Handle OPTIONS preflight (CORS / Private Network Access).
    if request.starts_with("OPTIONS ") {
        let preflight = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\nAccess-Control-Allow-Private-Network: true\r\nContent-Length: 0\r\n\r\n";
        let _ = stream.write_all(preflight.as_bytes());
        let _ = stream.flush();
        return;
    }

    if !is_upgrade {
        serve_http(&mut stream, &request);
        return;
    }

    handle_ws_upgrade(stream, request, tx, state, mesh_json);
}

/// Serve the full WASM REPL from the same port as the WebSocket.
/// This makes the WS connection same-origin, bypassing browser security policies.
fn serve_http(stream: &mut TcpStream, request: &str) {
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let (content_type, body): (&str, &[u8]) = match path {
        "/" | "/index.html" => (
            "text/html; charset=utf-8",
            include_bytes!("../../web/index.html"),
        ),
        "/unit.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("../../web/unit.js"),
        ),
        "/unit.wasm" => ("application/wasm", include_bytes!("../../web/unit.wasm")),
        _ => {
            let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes());
            return;
        }
    };
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
        content_type, body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn handle_ws_upgrade(
    mut stream: TcpStream,
    request: String,
    tx: std::sync::mpsc::Sender<WsEvent>,
    state: Arc<Mutex<WsBridgeState>>,
    mesh_json: Arc<Mutex<String>>,
) {
    // Extract Sec-WebSocket-Key.
    let key = request
        .lines()
        .find(|l| l.to_lowercase().starts_with("sec-websocket-key:"))
        .and_then(|l| l.split(':').nth(1))
        .map(|k| k.trim().to_string());

    let key = match key {
        Some(k) => k,
        None => return,
    };

    // Extract Origin for CORS.
    let _origin = request
        .lines()
        .find(|l| l.to_lowercase().starts_with("origin:"))
        .and_then(|l| l.split_once(':').map(|x| x.1))
        .map(|o| o.trim().to_string())
        .unwrap_or_else(|| "*".to_string());

    // Send upgrade response.
    let accept = ws_accept_key(&key);
    let response = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", accept);
    if stream.write_all(response.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();

    // Switch to short timeout for the frame read loop.
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok();

    // Generate client ID.
    let client_id = format!(
        "browser-{:04x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            & 0xFFFF
    );

    // Register client.
    {
        let mut st = state.lock().unwrap();
        st.clients.insert(
            client_id.clone(),
            WsClient {
                id: client_id.clone(),
                connected_at: Instant::now(),
                last_heartbeat: Instant::now(),
                fitness: 0,
            },
        );
    }
    let _ = tx.send(WsEvent::ClientConnected {
        id: client_id.clone(),
    });

    // Send a welcome frame immediately.
    {
        let welcome = r#"{"type":"welcome","id":"server"}"#;
        let frame = ws_encode_text(welcome);
        if stream.write_all(&frame).is_err() {
            return;
        }
        let _ = stream.flush();
    }

    // Also send initial mesh state if available.
    {
        let json = mesh_json.lock().unwrap().clone();
        if !json.is_empty() {
            let frame = ws_encode_text(&json);
            let _ = stream.write_all(&frame);
        }
    }

    let mut read_buf = Vec::new();
    let mut last_mesh_push = Instant::now();
    loop {
        let mut tmp = [0u8; 4096];
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                if tmp[0] & 0x0F == 8 {
                    break;
                }
                read_buf.extend_from_slice(&tmp[..n]);
                while let Some((text, consumed)) = ws_decode_frame(&read_buf) {
                    read_buf.drain(..consumed);
                    handle_browser_message(&text, &client_id, &tx, &state);
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }

        // Handle ping (opcode 9) in the raw buffer.
        if read_buf.first().map(|b| b & 0x0F == 9).unwrap_or(false) {
            let pong = ws_encode_pong(&[]);
            let _ = stream.write_all(&pong);
            if !read_buf.is_empty() {
                read_buf.clear();
            }
        }

        // Periodically push mesh state to browser (every 2s).
        if last_mesh_push.elapsed() >= Duration::from_secs(2) {
            let json = mesh_json.lock().unwrap().clone();
            if !json.is_empty() {
                let frame = ws_encode_text(&json);
                if stream.write_all(&frame).is_err() {
                    break;
                }
            }
            last_mesh_push = Instant::now();
            state.lock().unwrap().messages_relayed += 1;
        }
    }

    // Cleanup.
    {
        let mut st = state.lock().unwrap();
        st.clients.remove(&client_id);
    }
    let _ = tx.send(WsEvent::ClientDisconnected { id: client_id });
}

fn handle_browser_message(
    text: &str,
    client_id: &str,
    tx: &std::sync::mpsc::Sender<WsEvent>,
    state: &Arc<Mutex<WsBridgeState>>,
) {
    // Simple JSON parsing without serde. Look for "type" field.
    let msg_type = extract_json_string(text, "type").unwrap_or_default();

    match msg_type.as_str() {
        "heartbeat" => {
            let fitness = extract_json_number(text, "fitness").unwrap_or(0);
            let _ = tx.send(WsEvent::Heartbeat {
                id: client_id.to_string(),
                fitness,
            });
            if let Ok(mut st) = state.lock() {
                if let Some(c) = st.clients.get_mut(client_id) {
                    c.last_heartbeat = Instant::now();
                    c.fitness = fitness;
                }
                st.messages_relayed += 1;
            }
        }
        "goal_submit" => {
            let code = extract_json_string(text, "code").unwrap_or_default();
            let priority = extract_json_number(text, "priority").unwrap_or(5);
            let _ = tx.send(WsEvent::GoalSubmit { code, priority });
            if let Ok(mut st) = state.lock() {
                st.messages_relayed += 1;
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Minimal JSON helpers (no serde dependency)
// ---------------------------------------------------------------------------

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let pos = json.find(&pattern)?;
    let rest = &json[pos + pattern.len()..];
    // Skip : and whitespace, find opening quote.
    let rest = rest.trim_start().strip_prefix(':')?;
    let rest = rest.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_json_number(json: &str, key: &str) -> Option<i64> {
    let pattern = format!("\"{}\"", key);
    let pos = json.find(&pattern)?;
    let rest = &json[pos + pattern.len()..];
    let rest = rest.trim_start().strip_prefix(':')?;
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Parameters for building the mesh JSON state.
pub struct MeshJsonParams<'a> {
    pub self_id: &'a str,
    pub self_fitness: i64,
    pub self_generation: u32,
    pub peers: &'a [(String, i64, String)], // (id_hex, fitness, addr)
    pub goals: (usize, usize, usize, usize), // (total, pending, active, completed)
    pub recent_events: &'a [String],
    pub children: &'a [(String, u32)], // (id_hex, generation)
    pub watch_count: usize,
    pub alert_count: usize,
}

/// Build a JSON mesh state string for pushing to browsers.
/// Build a rich JSON mesh state for the visualizer.
pub fn build_mesh_json(params: MeshJsonParams) -> String {
    let self_id = params.self_id;
    let self_fitness = params.self_fitness;
    let self_generation = params.self_generation;
    let peers = params.peers;
    let goals = params.goals;
    let recent_events = params.recent_events;
    let children = params.children;
    let watch_count = params.watch_count;
    let alert_count = params.alert_count;
    let mut json = format!(
        r#"{{"type":"mesh_state","self_id":"{}","self_fitness":{},"self_generation":{},"#,
        self_id, self_fitness, self_generation
    );
    // Peers array.
    json.push_str(r#""peers":["#);
    for (i, (id, fit, addr)) in peers.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"id":"{}","fitness":{},"addr":"{}"}}"#,
            id, fit, addr
        ));
    }
    json.push_str(r#"],"#);
    // Goals.
    json.push_str(&format!(
        r#""goals":{{"total":{},"pending":{},"active":{},"completed":{}}},"#,
        goals.0, goals.1, goals.2, goals.3
    ));
    // Recent events.
    json.push_str(r#""recent_events":["#);
    for (i, evt) in recent_events.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        let escaped = evt.replace('\\', "\\\\").replace('"', "\\\"");
        json.push_str(&format!(r#""{}""#, escaped));
    }
    json.push_str(r#"],"#);
    // Children.
    json.push_str(r#""children":["#);
    for (i, (id, gen)) in children.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(r#"{{"id":"{}","generation":{}}}"#, id, gen));
    }
    json.push_str(r#"],"#);
    // Counts.
    json.push_str(&format!(
        r#""watch_count":{},"alert_count":{}}}"#,
        watch_count, alert_count
    ));
    json
}
