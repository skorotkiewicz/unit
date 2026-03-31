// mesh.rs — Mesh networking and consensus for unit
//
// Gossip-based peer discovery over UDP unicast. Each unit has a unique ID,
// sends periodic heartbeats, and maintains a peer table. Replication
// decisions are made through a simple majority-vote consensus protocol.
//
// Architecture:
//   - Main thread: runs the Forth REPL, calls MeshNode methods synchronously
//   - Network thread: UDP recv loop, heartbeats, vote processing, timeouts
//   - Shared state: Arc<Mutex<MeshState>> bridges the two threads
//
// Configuration via environment variables:
//   UNIT_PORT  — UDP port to bind (default: 0 = OS-assigned)
//   UNIT_PEERS — comma-separated seed peers, e.g. "127.0.0.1:4202"

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::features::fitness::PeerFitness;
use crate::goals::{Goal, GoalId, GoalRegistry, GoalStatus, Task, TaskId, TaskResult, TaskStatus};
use crate::types::{Cell, Entry, Instruction};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const PEER_TIMEOUT: Duration = Duration::from_secs(15);
const PROPOSAL_TIMEOUT: Duration = Duration::from_secs(5);
const PROPOSAL_COOLDOWN: Duration = Duration::from_secs(10);
const RECV_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_GOSSIP_PEERS: usize = 8;
const DEFAULT_CAPACITY: u32 = 100;

// Message type tags.
const MSG_HEARTBEAT: u8 = 1;
const MSG_PROPOSE: u8 = 2;
const MSG_VOTE: u8 = 3;
const MSG_COMMIT: u8 = 4;
const MSG_REJECT: u8 = 5;
const MSG_DATA: u8 = 6;
const MSG_GOAL_BROADCAST: u8 = 7;
const MSG_TASK_CLAIM: u8 = 8;
const MSG_TASK_RESULT: u8 = 9;
const MSG_WORD_SHARE: u8 = 10;
const MSG_DISCOVERY_BEACON: u8 = 11;
const MSG_SPAWN_INTENT: u8 = 12;
const MSG_CULL_INTENT: u8 = 13;
const MSG_REPLICATE_REQUEST: u8 = 14;
const MSG_REPLICATE_ACCEPT: u8 = 15;
const MSG_REPLICATE_DENY: u8 = 16;

// Wire format magic.
const MAGIC: &[u8; 4] = b"UNIT";

// Discovery beacon port (all units listen on this for LAN discovery).
const DISCOVERY_PORT: u16 = 4200;
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Node identity
// ---------------------------------------------------------------------------

pub type NodeId = [u8; 8];

/// Generate a random node ID from /dev/urandom (with time-based fallback).
fn generate_id() -> NodeId {
    let mut id = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut id);
    } else {
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        id = t.to_ne_bytes();
    }
    id
}

pub fn id_to_hex(id: &NodeId) -> String {
    id.iter().map(|b| format!("{:02x}", b)).collect()
}

// ---------------------------------------------------------------------------
// Wire format helpers — big-endian encoding/decoding
// ---------------------------------------------------------------------------

fn write_u8(buf: &mut Vec<u8>, v: u8) {
    buf.push(v);
}
fn write_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_be_bytes());
}
fn write_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_be_bytes());
}
fn write_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_be_bytes());
}
fn write_i64(buf: &mut Vec<u8>, v: i64) {
    buf.extend_from_slice(&v.to_be_bytes());
}
fn write_bytes(buf: &mut Vec<u8>, data: &[u8]) {
    buf.extend_from_slice(data);
}

fn read_u8(data: &[u8], pos: &mut usize) -> Option<u8> {
    if *pos >= data.len() {
        return None;
    }
    let v = data[*pos];
    *pos += 1;
    Some(v)
}
fn read_u16(data: &[u8], pos: &mut usize) -> Option<u16> {
    if *pos + 2 > data.len() {
        return None;
    }
    let v = u16::from_be_bytes([data[*pos], data[*pos + 1]]);
    *pos += 2;
    Some(v)
}
fn read_u32(data: &[u8], pos: &mut usize) -> Option<u32> {
    if *pos + 4 > data.len() {
        return None;
    }
    let v = u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Some(v)
}
fn read_u64(data: &[u8], pos: &mut usize) -> Option<u64> {
    if *pos + 8 > data.len() {
        return None;
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[*pos..*pos + 8]);
    *pos += 8;
    Some(u64::from_be_bytes(bytes))
}
fn read_i64(data: &[u8], pos: &mut usize) -> Option<i64> {
    if *pos + 8 > data.len() {
        return None;
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[*pos..*pos + 8]);
    *pos += 8;
    Some(i64::from_be_bytes(bytes))
}
fn read_bytes(data: &[u8], pos: &mut usize, len: usize) -> Option<Vec<u8>> {
    if *pos + len > data.len() {
        return None;
    }
    let v = data[*pos..*pos + len].to_vec();
    *pos += len;
    Some(v)
}

// ---------------------------------------------------------------------------
// Message header: MAGIC(4) + type(1) + sender_id(8) + sender_port(2)
// ---------------------------------------------------------------------------

const HEADER_SIZE: usize = 4 + 1 + 8 + 2; // 15 bytes

fn encode_header(buf: &mut Vec<u8>, msg_type: u8, id: &NodeId, port: u16) {
    write_bytes(buf, MAGIC);
    write_u8(buf, msg_type);
    write_bytes(buf, id);
    write_u16(buf, port);
}

/// Returns (msg_type, sender_id, sender_port) or None if invalid.
fn decode_header(data: &[u8], pos: &mut usize) -> Option<(u8, NodeId, u16)> {
    let magic = read_bytes(data, pos, 4)?;
    if magic != MAGIC {
        return None;
    }
    let msg_type = read_u8(data, pos)?;
    let id_bytes = read_bytes(data, pos, 8)?;
    let mut id = [0u8; 8];
    id.copy_from_slice(&id_bytes);
    let port = read_u16(data, pos)?;
    Some((msg_type, id, port))
}

// ---------------------------------------------------------------------------
// Peer info
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct PeerInfo {
    addr: SocketAddr,
    id: NodeId,
    load: u32,
    capacity: u32,
    peer_count: u16,
    fitness: i64,
    last_seen: Instant,
}

// ---------------------------------------------------------------------------
// Proposal tracking
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Proposal {
    id: u64,
    proposer: NodeId,
    reason: String,
    votes_yes: HashSet<NodeId>,
    votes_no: HashSet<NodeId>,
    started: Instant,
    total_peers_at_start: usize,
    committed: bool,
    /// Serialized state to send on commit (only set on the proposer).
    state_bytes: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// Inbox message (for Forth SEND/RECV)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct InboxMessage {
    pub from: NodeId,
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Shared state between VM thread and network thread
// ---------------------------------------------------------------------------

pub(crate) struct MeshState {
    id: NodeId,
    port: u16,
    peers: HashMap<NodeId, PeerInfo>,
    inbox: VecDeque<InboxMessage>,
    /// Active proposals (keyed by proposal ID).
    proposals: HashMap<u64, Proposal>,
    /// Timestamp of the last completed/failed proposal by this node.
    last_proposal_time: Option<Instant>,
    /// This node's current load metric.
    load: u32,
    /// This node's capacity threshold.
    capacity: u32,
    /// Log of recent mesh events (ring buffer, for MESH-STATUS).
    event_log: VecDeque<String>,
    /// Goal and task registry, shared across the mesh via gossip.
    pub(crate) goals: GoalRegistry,
    /// This unit's fitness score (updated by VM, included in heartbeats).
    fitness: i64,
    /// Flag: network thread detected that auto-replication is needed.
    auto_replicate_needed: bool,
    /// Flag to stop the network thread.
    running: bool,
    // --- Swarm autonomy ---
    /// Words shared from peers: (name, origin_id).
    pub(crate) shared_words: Vec<(String, NodeId)>,
    /// Pending word shares from network thread → VM thread.
    pub(crate) word_inbox: VecDeque<SharedWord>,
    /// Swarm configuration.
    pub(crate) auto_discover: bool,
    pub(crate) auto_share: bool,
    pub(crate) auto_spawn: bool,
    pub(crate) auto_cull: bool,
    pub(crate) min_units: usize,
    pub(crate) max_units: usize,
    /// Pending spawn/cull intents from peers (for coordination).
    pub(crate) spawn_intent_active: bool,
    pub(crate) cull_intent_active: bool,
    // --- Replication consent ---
    pub(crate) trust_level: TrustLevel,
    pub(crate) pending_requests: Vec<ReplicationRequest>,
    pub(crate) replication_log: Vec<ReplicationLogEntry>,
    pub(crate) next_request_id: u32,
    pub(crate) parent_id: Option<NodeId>,
    pub(crate) children_ids: Vec<NodeId>,
}

/// A word definition received from a peer.
#[derive(Clone, Debug)]
pub struct SharedWord {
    pub name: String,
    pub body_source: String,
    pub origin: NodeId,
}

/// Trust level for replication consent.
#[derive(Clone, Debug, PartialEq)]
pub enum TrustLevel {
    All,    // auto-accept everything (default)
    Mesh,   // auto-accept known peers, prompt for unknown
    Family, // auto-accept parent/children/siblings, prompt for others
    None,   // prompt for everything
}

impl TrustLevel {
    pub fn label(&self) -> &str {
        match self {
            TrustLevel::All => "all",
            TrustLevel::Mesh => "mesh",
            TrustLevel::Family => "family",
            TrustLevel::None => "none",
        }
    }
    pub fn as_val(&self) -> i64 {
        match self {
            TrustLevel::All => 0,
            TrustLevel::Mesh => 1,
            TrustLevel::Family => 2,
            TrustLevel::None => 3,
        }
    }
}

/// A pending replication request from a remote peer.
#[derive(Clone, Debug)]
pub struct ReplicationRequest {
    pub id: u32,
    pub sender_id: NodeId,
    pub sender_fitness: i64,
    pub sender_generation: u32,
    pub package_size: u64,
    pub reason: String,
    pub received_at: Instant,
}

/// An entry in the replication log.
#[derive(Clone, Debug)]
pub struct ReplicationLogEntry {
    pub timestamp: u64,
    pub direction: String, // "incoming" or "outgoing"
    pub peer_id: NodeId,
    pub reason: String,
    pub result: String, // "accepted", "denied", "expired", "auto-accepted"
}

impl MeshState {
    fn log_event(&mut self, msg: String) {
        self.event_log.push_back(msg);
        if self.event_log.len() > 20 {
            self.event_log.pop_front();
        }
    }
}

// ---------------------------------------------------------------------------
// MeshNode — public API for the VM
// ---------------------------------------------------------------------------

pub struct MeshNode {
    id: NodeId,
    id_hex: String,
    socket: UdpSocket,
    state: Arc<Mutex<MeshState>>,
    _thread: Option<thread::JoinHandle<()>>,
    /// Receiver for incoming replication packages (from TCP listener).
    repl_rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
    /// TCP port for replication listener.
    pub repl_port: u16,
}

impl Drop for MeshNode {
    fn drop(&mut self) {
        if let Ok(mut st) = self.state.lock() {
            st.running = false;
        }
        if let Some(handle) = self._thread.take() {
            let _ = handle.join();
        }
    }
}

impl MeshNode {
    /// Start the mesh node. Binds a UDP socket and spawns the network thread.
    pub fn start(port: u16, seed_peers: Vec<SocketAddr>) -> Result<Self, String> {
        Self::start_with_id(None, port, seed_peers)
    }

    pub fn start_with_id(
        fixed_id: Option<NodeId>,
        port: u16,
        seed_peers: Vec<SocketAddr>,
    ) -> Result<Self, String> {
        let id = fixed_id.unwrap_or_else(generate_id);
        let id_hex = id_to_hex(&id);

        let bind_addr = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), port);
        let socket = UdpSocket::bind(bind_addr).map_err(|e| format!("bind: {}", e))?;
        let local_port = socket.local_addr().map_err(|e| format!("{}", e))?.port();

        let state = Arc::new(Mutex::new(MeshState {
            id,
            port: local_port,
            peers: HashMap::new(),
            inbox: VecDeque::new(),
            proposals: HashMap::new(),
            last_proposal_time: None,
            load: 0,
            capacity: DEFAULT_CAPACITY,
            event_log: VecDeque::new(),
            goals: GoalRegistry::new(&id),
            fitness: 0,
            auto_replicate_needed: false,
            running: true,
            shared_words: Vec::new(),
            word_inbox: VecDeque::new(),
            auto_discover: true,
            auto_share: false,
            auto_spawn: false,
            auto_cull: false,
            min_units: 1,
            max_units: 10,
            spawn_intent_active: false,
            cull_intent_active: false,
            trust_level: TrustLevel::All,
            pending_requests: Vec::new(),
            replication_log: Vec::new(),
            next_request_id: 1,
            parent_id: None,
            children_ids: Vec::new(),
        }));

        // Add seed peers as tentative entries so the first heartbeat reaches them.
        {
            let mut st = state.lock().unwrap();
            for addr in &seed_peers {
                // We don't know their ID yet; use a placeholder that will be
                // replaced when their first heartbeat arrives. We key by addr
                // temporarily by generating a deterministic pseudo-ID from addr.
                let pseudo_id = addr_to_pseudo_id(addr);
                st.peers.insert(
                    pseudo_id,
                    PeerInfo {
                        addr: *addr,
                        id: pseudo_id,
                        load: 0,
                        capacity: 0,
                        peer_count: 0,
                        fitness: 0,
                        last_seen: Instant::now(),
                    },
                );
            }
            st.log_event(format!(
                "mesh started on port {} with {} seed peers",
                local_port,
                seed_peers.len()
            ));
        }

        let thread_socket = socket.try_clone().map_err(|e| format!("clone: {}", e))?;
        let thread_state = Arc::clone(&state);
        let thread_id = id;

        let handle = thread::spawn(move || {
            network_thread(thread_socket, thread_state, thread_id);
        });

        // Start TCP replication listener on port+1000 (best effort).
        let repl_tcp_port = if local_port > 0 { local_port + 1000 } else { 0 };
        let (repl_rx, actual_repl_port) =
            match crate::spawn::start_replication_listener(repl_tcp_port) {
                Ok(rx) => (Some(rx), repl_tcp_port),
                Err(_) => (None, 0),
            };

        // Start discovery beacon listener (best effort — port may be in use).
        Self::start_discovery_listener(Arc::clone(&state), id);

        Ok(MeshNode {
            id,
            id_hex,
            socket,
            state,
            _thread: Some(handle),
            repl_rx,
            repl_port: actual_repl_port,
        })
    }

    pub fn id_hex(&self) -> &str {
        &self.id_hex
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }

    pub fn local_port(&self) -> u16 {
        self.socket.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    pub fn peer_count(&self) -> usize {
        let st = self.state.lock().unwrap();
        st.peers.len()
    }

    pub fn load(&self) -> u32 {
        self.state.lock().unwrap().load
    }

    pub fn set_load(&self, load: u32) {
        self.state.lock().unwrap().load = load;
    }

    pub fn capacity(&self) -> u32 {
        self.state.lock().unwrap().capacity
    }

    /// Print mesh status to stdout.
    pub fn format_status(&self) -> String {
        let st = self.state.lock().unwrap();
        let mut out = String::from("--- mesh status ---\n");
        out.push_str(&format!("id:       {}\n", id_to_hex(&st.id)));
        out.push_str(&format!("port:     {}\n", st.port));
        out.push_str(&format!("load:     {}/{}\n", st.load, st.capacity));
        out.push_str(&format!("peers:    {}\n", st.peers.len()));
        for peer in st.peers.values() {
            let age = peer.last_seen.elapsed().as_secs();
            out.push_str(&format!(
                "  {} @ {} load={}/{} seen={}s ago\n",
                id_to_hex(&peer.id),
                peer.addr,
                peer.load,
                peer.capacity,
                age
            ));
        }
        if !st.proposals.is_empty() {
            out.push_str("proposals:\n");
            for (pid, prop) in &st.proposals {
                out.push_str(&format!(
                    "  #{:016x} by {} yes={} no={} committed={}\n",
                    pid,
                    id_to_hex(&prop.proposer),
                    prop.votes_yes.len(),
                    prop.votes_no.len(),
                    prop.committed
                ));
            }
        }
        if !st.event_log.is_empty() {
            out.push_str("recent events:\n");
            for evt in &st.event_log {
                out.push_str(&format!("  {}\n", evt));
            }
        }
        out.push_str("---\n");
        out
    }

    /// Send a data message (from Forth SEND word) to all peers.
    pub fn send_data(&self, data: &[u8]) {
        let st = self.state.lock().unwrap();
        let port = st.port;
        let peers: Vec<SocketAddr> = st.peers.values().map(|p| p.addr).collect();
        drop(st);

        let mut buf = Vec::with_capacity(HEADER_SIZE + 2 + data.len());
        encode_header(&mut buf, MSG_DATA, &self.id, port);
        write_u16(&mut buf, data.len() as u16);
        write_bytes(&mut buf, data);

        for addr in &peers {
            let _ = self.socket.send_to(&buf, addr);
        }
    }

    /// Pop the next received data message from the inbox.
    pub fn recv_data(&self) -> Option<InboxMessage> {
        self.state.lock().unwrap().inbox.pop_front()
    }

    /// Propose replication to the mesh. Returns an error if on cooldown or
    /// if there are no peers to vote.
    pub fn propose_replicate(&self, reason: &str, state_bytes: Vec<u8>) -> Result<(), String> {
        let mut st = self.state.lock().unwrap();

        // Anti-spam: check cooldown.
        if let Some(last) = st.last_proposal_time {
            if last.elapsed() < PROPOSAL_COOLDOWN {
                let remaining = PROPOSAL_COOLDOWN - last.elapsed();
                return Err(format!("cooldown: wait {}s", remaining.as_secs()));
            }
        }

        // Anti-spam: only one active proposal per node.
        for prop in st.proposals.values() {
            if prop.proposer == self.id
                && !prop.committed
                && prop.started.elapsed() < PROPOSAL_TIMEOUT
            {
                return Err("already have an active proposal".into());
            }
        }

        if st.peers.is_empty() {
            return Err("no peers to vote".into());
        }

        // Generate proposal ID from time.
        let proposal_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let total_peers = st.peers.len();
        let port = st.port;
        let peers: Vec<SocketAddr> = st.peers.values().map(|p| p.addr).collect();

        let proposal = Proposal {
            id: proposal_id,
            proposer: self.id,
            reason: reason.to_string(),
            votes_yes: HashSet::new(),
            votes_no: HashSet::new(),
            started: Instant::now(),
            total_peers_at_start: total_peers,
            committed: false,
            state_bytes: Some(state_bytes),
        };

        st.proposals.insert(proposal_id, proposal);
        st.log_event(format!(
            "proposed replication #{:016x}: {}",
            proposal_id, reason
        ));
        drop(st);

        // Broadcast PROPOSE to all peers.
        let reason_bytes = reason.as_bytes();
        let mut buf = Vec::with_capacity(HEADER_SIZE + 8 + 2 + reason_bytes.len());
        encode_header(&mut buf, MSG_PROPOSE, &self.id, port);
        write_u64(&mut buf, proposal_id);
        write_u16(&mut buf, reason_bytes.len() as u16);
        write_bytes(&mut buf, reason_bytes);

        for addr in &peers {
            let _ = self.socket.send_to(&buf, addr);
        }

        Ok(())
    }

    /// Shut down the mesh (stops network thread).
    pub fn shutdown(&self) {
        if let Ok(mut st) = self.state.lock() {
            st.running = false;
        }
    }

    // -------------------------------------------------------------------
    // Goal and task operations
    // -------------------------------------------------------------------

    /// Create a goal and broadcast it to all peers.
    /// If `code` is Some, the goal carries executable Forth code.
    pub fn create_goal(&self, description: &str, priority: Cell, code: Option<String>) -> GoalId {
        let mut st = self.state.lock().unwrap();
        let goal_id = st
            .goals
            .create_goal(description.to_string(), priority, self.id, code);
        st.log_event(format!("goal #{} created: {}", goal_id, description));

        // Get the goal and its tasks for broadcasting.
        let goal = st.goals.goals.get(&goal_id).cloned();
        let tasks: Vec<Task> = goal
            .as_ref()
            .map(|g| {
                g.task_ids
                    .iter()
                    .filter_map(|tid| st.goals.tasks.get(tid).cloned())
                    .collect()
            })
            .unwrap_or_default();
        let port = st.port;
        let peers: Vec<SocketAddr> = st.peers.values().map(|p| p.addr).collect();
        drop(st);

        // Broadcast the goal to all peers.
        if let Some(goal) = goal {
            let buf = encode_goal_broadcast(&self.id, port, &goal, &tasks);
            for addr in &peers {
                let _ = self.socket.send_to(&buf, addr);
            }
        }

        goal_id
    }

    /// Claim the next available task. Returns (task_id, goal_id, description).
    pub fn claim_task(&self) -> Option<(TaskId, GoalId, String)> {
        let mut st = self.state.lock().unwrap();
        let result = st.goals.claim_task(self.id);

        if let Some((task_id, goal_id, ref desc)) = result {
            st.log_event(format!(
                "claimed task #{} (goal #{}): {}",
                task_id, goal_id, desc
            ));
            let port = st.port;
            let peers: Vec<SocketAddr> = st.peers.values().map(|p| p.addr).collect();
            drop(st);

            // Broadcast claim.
            let mut buf = Vec::with_capacity(HEADER_SIZE + 16);
            encode_header(&mut buf, MSG_TASK_CLAIM, &self.id, port);
            write_u64(&mut buf, task_id);
            write_u64(&mut buf, goal_id);
            for addr in &peers {
                let _ = self.socket.send_to(&buf, addr);
            }
            return Some((task_id, goal_id, desc.clone()));
        }
        None
    }

    /// Complete a task with a full result and broadcast.
    pub fn complete_task_with_result(&self, task_id: TaskId, result: TaskResult) {
        let mut st = self.state.lock().unwrap();
        let goal_id = st.goals.tasks.get(&task_id).map(|t| t.goal_id);
        let success = result.success;
        st.goals.complete_task(task_id, Some(result.clone()));
        st.log_event(format!(
            "task #{} {} (stack={} output={}b)",
            task_id,
            if success { "completed" } else { "failed" },
            result.stack_snapshot.len(),
            result.output.len(),
        ));
        let port = st.port;
        let peers: Vec<SocketAddr> = st.peers.values().map(|p| p.addr).collect();
        drop(st);

        // Broadcast result with full TaskResult.
        let output_bytes = result.output.as_bytes();
        let error_bytes = result.error.as_deref().unwrap_or("").as_bytes();
        let mut buf = Vec::with_capacity(HEADER_SIZE + 32 + output_bytes.len());
        encode_header(&mut buf, MSG_TASK_RESULT, &self.id, port);
        write_u64(&mut buf, task_id);
        write_u64(&mut buf, goal_id.unwrap_or(0));
        write_u8(&mut buf, if success { 1 } else { 0 });
        write_u16(&mut buf, result.stack_snapshot.len() as u16);
        for &val in &result.stack_snapshot {
            write_i64(&mut buf, val);
        }
        write_u16(&mut buf, output_bytes.len() as u16);
        write_bytes(&mut buf, output_bytes);
        write_u16(&mut buf, error_bytes.len() as u16);
        write_bytes(&mut buf, error_bytes);
        for addr in &peers {
            let _ = self.socket.send_to(&buf, addr);
        }
    }

    /// Claim the next available executable task.
    pub fn claim_executable_task(&self) -> Option<(TaskId, GoalId, String, String)> {
        let mut st = self.state.lock().unwrap();
        let result = st.goals.claim_executable_task(self.id);

        if let Some((task_id, goal_id, ref desc, _)) = result {
            st.log_event(format!(
                "auto-claimed task #{} (goal #{})",
                task_id, goal_id
            ));
            let port = st.port;
            let peers: Vec<SocketAddr> = st.peers.values().map(|p| p.addr).collect();
            drop(st);

            // Broadcast claim.
            let mut buf = Vec::with_capacity(HEADER_SIZE + 16);
            encode_header(&mut buf, MSG_TASK_CLAIM, &self.id, port);
            write_u64(&mut buf, task_id);
            write_u64(&mut buf, goal_id);
            for addr in &peers {
                let _ = self.socket.send_to(&buf, addr);
            }
            return Some((task_id, goal_id, desc.clone(), result.unwrap().3));
        }
        None
    }

    /// Cancel a goal and broadcast.
    pub fn cancel_goal(&self, goal_id: GoalId) -> bool {
        let mut st = self.state.lock().unwrap();
        let ok = st.goals.cancel_goal(goal_id);
        if ok {
            st.log_event(format!("goal #{} cancelled", goal_id));
            let goal = st.goals.goals.get(&goal_id).cloned();
            let tasks: Vec<Task> = goal
                .as_ref()
                .map(|g| {
                    g.task_ids
                        .iter()
                        .filter_map(|tid| st.goals.tasks.get(tid).cloned())
                        .collect()
                })
                .unwrap_or_default();
            let port = st.port;
            let peers: Vec<SocketAddr> = st.peers.values().map(|p| p.addr).collect();
            drop(st);

            if let Some(goal) = goal {
                let buf = encode_goal_broadcast(&self.id, port, &goal, &tasks);
                for addr in &peers {
                    let _ = self.socket.send_to(&buf, addr);
                }
            }
        }
        ok
    }

    /// Change a goal's priority and broadcast.
    pub fn steer_goal(&self, goal_id: GoalId, priority: Cell) -> bool {
        let mut st = self.state.lock().unwrap();
        let ok = st.goals.steer_goal(goal_id, priority);
        if ok {
            st.log_event(format!("goal #{} priority -> {}", goal_id, priority));
            let goal = st.goals.goals.get(&goal_id).cloned();
            let tasks: Vec<Task> = goal
                .as_ref()
                .map(|g| {
                    g.task_ids
                        .iter()
                        .filter_map(|tid| st.goals.tasks.get(tid).cloned())
                        .collect()
                })
                .unwrap_or_default();
            let port = st.port;
            let peers: Vec<SocketAddr> = st.peers.values().map(|p| p.addr).collect();
            drop(st);

            if let Some(goal) = goal {
                let buf = encode_goal_broadcast(&self.id, port, &goal, &tasks);
                for addr in &peers {
                    let _ = self.socket.send_to(&buf, addr);
                }
            }
        }
        ok
    }

    pub fn format_goals(&self) -> String {
        self.state.lock().unwrap().goals.format_goals()
    }

    pub fn format_tasks(&self) -> String {
        self.state.lock().unwrap().goals.format_my_tasks(&self.id)
    }

    pub fn format_goal_tasks(&self, goal_id: GoalId) -> String {
        self.state.lock().unwrap().goals.format_goal_tasks(goal_id)
    }

    pub fn format_report(&self) -> String {
        self.state.lock().unwrap().goals.format_report()
    }

    pub fn format_task_result(&self, task_id: TaskId) -> String {
        self.state.lock().unwrap().goals.format_task_result(task_id)
    }

    pub fn format_goal_result(&self, goal_id: GoalId) -> String {
        self.state.lock().unwrap().goals.format_goal_result(goal_id)
    }

    pub fn pending_goal_count(&self) -> usize {
        self.state.lock().unwrap().goals.active_goal_count()
    }

    pub fn should_auto_replicate(&self) -> bool {
        self.state.lock().unwrap().auto_replicate_needed
    }

    pub fn clear_auto_replicate(&self) {
        self.state.lock().unwrap().auto_replicate_needed = false;
    }

    /// Clone the goal registry (for serialization during replication).
    pub fn clone_goals(&self) -> GoalRegistry {
        self.state.lock().unwrap().goals.clone()
    }

    /// Get the code payload for a goal, if it has one.
    pub fn goal_code(&self, goal_id: GoalId) -> Option<String> {
        self.state.lock().unwrap().goals.goal_code(goal_id)
    }

    /// Get this node's ID as raw bytes.
    pub fn id_bytes(&self) -> NodeId {
        self.id
    }

    /// Update the fitness score in shared state (called by VM after tasks).
    pub fn set_fitness(&self, score: i64) {
        self.state.lock().unwrap().fitness = score;
    }

    /// Check for an incoming replication package (non-blocking).
    pub fn recv_replication(&self) -> Option<Vec<u8>> {
        self.repl_rx.as_ref().and_then(|rx| rx.try_recv().ok())
    }

    /// Lock the shared state for direct access.
    pub(crate) fn state_lock(&self) -> std::sync::MutexGuard<'_, MeshState> {
        self.state.lock().unwrap()
    }

    /// Get all peer fitness scores for the leaderboard.
    pub fn peer_fitness_list(&self) -> Vec<PeerFitness> {
        let st = self.state.lock().unwrap();
        st.peers
            .values()
            .map(|p| PeerFitness {
                id: p.id,
                score: p.fitness,
            })
            .collect()
    }

    /// Get detailed peer info for the visualizer.
    pub fn peer_details(&self) -> Vec<(String, i64, String)> {
        let st = self.state.lock().unwrap();
        st.peers
            .values()
            .map(|p| (id_to_hex(&p.id), p.fitness, p.addr.to_string()))
            .collect()
    }

    /// Get goal statistics.
    pub fn goal_stats(&self) -> (usize, usize, usize, usize) {
        let st = self.state.lock().unwrap();
        let total = st.goals.goals.len();
        let pending = st
            .goals
            .goals
            .values()
            .filter(|g| g.status == crate::goals::GoalStatus::Pending)
            .count();
        let active = st
            .goals
            .goals
            .values()
            .filter(|g| g.status == crate::goals::GoalStatus::Active)
            .count();
        let completed = st
            .goals
            .goals
            .values()
            .filter(|g| g.status == crate::goals::GoalStatus::Completed)
            .count();
        (total, pending, active, completed)
    }

    /// Drain recent events from the event log (returns up to 20, clearing them).
    pub fn drain_recent_events(&self) -> Vec<String> {
        let mut st = self.state.lock().unwrap();
        let events: Vec<String> = st.event_log.drain(..).collect();
        events.into_iter().rev().take(20).collect()
    }

    // -------------------------------------------------------------------
    // Discovery
    // -------------------------------------------------------------------

    /// Send a discovery beacon via UDP broadcast.
    pub fn send_discovery_beacon(&self) {
        let st = self.state.lock().unwrap();
        if !st.auto_discover {
            return;
        }
        let port = st.port;
        drop(st);

        let mut buf = Vec::with_capacity(HEADER_SIZE + 2);
        encode_header(&mut buf, MSG_DISCOVERY_BEACON, &self.id, port);
        // Broadcast to the discovery port on the local network.
        let broadcast_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", DISCOVERY_PORT).parse().unwrap();
        let _ = self.socket.send_to(&buf, broadcast_addr);
    }

    /// Start a discovery listener on the shared beacon port.
    pub(crate) fn start_discovery_listener(state: Arc<Mutex<MeshState>>, my_id: NodeId) {
        let addr = format!("0.0.0.0:{}", DISCOVERY_PORT);
        let sock = match UdpSocket::bind(&addr) {
            Ok(s) => s,
            Err(_) => return, // port already in use (another unit is listening)
        };
        sock.set_read_timeout(Some(Duration::from_millis(500))).ok();
        // Allow multiple units to bind (SO_REUSEADDR is set by default on some OSes).

        std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            loop {
                {
                    let st = state.lock().unwrap();
                    if !st.running {
                        return;
                    }
                }
                if let Ok((len, src)) = sock.recv_from(&mut buf) {
                    if len < HEADER_SIZE {
                        continue;
                    }
                    if &buf[0..4] != MAGIC {
                        continue;
                    }
                    let msg_type = buf[4];
                    if msg_type != MSG_DISCOVERY_BEACON {
                        continue;
                    }
                    let mut sender_id = [0u8; 8];
                    sender_id.copy_from_slice(&buf[5..13]);
                    if sender_id == my_id {
                        continue;
                    } // ignore own beacon
                    let sender_port = u16::from_be_bytes([buf[13], buf[14]]);
                    let peer_addr: std::net::SocketAddr =
                        format!("{}:{}", src.ip(), sender_port).parse().unwrap();

                    let mut st = state.lock().unwrap();
                    if let std::collections::hash_map::Entry::Vacant(e) = st.peers.entry(sender_id)
                    {
                        e.insert(PeerInfo {
                            addr: peer_addr,
                            id: sender_id,
                            load: 0,
                            capacity: 0,
                            peer_count: 0,
                            fitness: 0,
                            last_seen: Instant::now(),
                        });
                        st.log_event(format!(
                            "discovered {} via beacon @ {}",
                            id_to_hex(&sender_id),
                            peer_addr
                        ));
                    }
                }
            }
        });
    }

    // -------------------------------------------------------------------
    // Word sharing
    // -------------------------------------------------------------------

    /// Broadcast a word definition to all peers.
    pub fn share_word(&self, name: &str, source: &str) {
        let st = self.state.lock().unwrap();
        let port = st.port;
        let peers: Vec<std::net::SocketAddr> = st.peers.values().map(|p| p.addr).collect();
        drop(st);

        let mut buf = Vec::with_capacity(HEADER_SIZE + 4 + name.len() + source.len());
        encode_header(&mut buf, MSG_WORD_SHARE, &self.id, port);
        let nb = name.as_bytes();
        write_u16(&mut buf, nb.len() as u16);
        write_bytes(&mut buf, nb);
        let sb = source.as_bytes();
        write_u16(&mut buf, sb.len() as u16);
        write_bytes(&mut buf, sb);
        for addr in &peers {
            let _ = self.socket.send_to(&buf, addr);
        }
    }

    /// Receive pending shared words (called by VM to compile them).
    pub fn recv_shared_words(&self) -> Vec<SharedWord> {
        let mut st = self.state.lock().unwrap();
        st.word_inbox.drain(..).collect()
    }

    /// Get list of words shared from peers.
    pub fn shared_words_list(&self) -> Vec<(String, String)> {
        let st = self.state.lock().unwrap();
        st.shared_words
            .iter()
            .map(|(name, origin)| (name.clone(), id_to_hex(origin)))
            .collect()
    }

    // -------------------------------------------------------------------
    // Swarm autonomy
    // -------------------------------------------------------------------

    /// Check if autonomous spawning should be triggered.
    pub fn should_auto_spawn(&self) -> bool {
        let st = self.state.lock().unwrap();
        if !st.auto_spawn {
            return false;
        }
        let units = st.peers.len() + 1;
        if units >= st.max_units {
            return false;
        }
        let pending = st.goals.pending_task_count();
        pending > units
    }

    /// Check if this unit should autonomously cull itself.
    pub fn should_auto_cull(&self) -> bool {
        let st = self.state.lock().unwrap();
        if !st.auto_cull {
            return false;
        }
        let units = st.peers.len() + 1;
        if units <= st.min_units {
            return false;
        }
        // Cull if fitness is below mesh average.
        let my_fitness = st.fitness;
        let total: i64 = st.peers.values().map(|p| p.fitness).sum::<i64>() + my_fitness;
        let avg = total / units as i64;
        my_fitness < avg - 10 // only cull if significantly below average
    }

    /// Format swarm status.
    pub fn format_swarm_status(&self) -> String {
        let st = self.state.lock().unwrap();
        let units = st.peers.len() + 1;
        format!(
            "--- swarm status ---\n\
             units: {}/{}-{}\n\
             auto-discover: {} auto-share: {} auto-spawn: {} auto-cull: {}\n\
             shared words: {} pending word inbox: {}\n\
             ---\n",
            units,
            st.min_units,
            st.max_units,
            if st.auto_discover { "ON" } else { "OFF" },
            if st.auto_share { "ON" } else { "OFF" },
            if st.auto_spawn { "ON" } else { "OFF" },
            if st.auto_cull { "ON" } else { "OFF" },
            st.shared_words.len(),
            st.word_inbox.len(),
        )
    }

    // -------------------------------------------------------------------
    // Replication consent
    // -------------------------------------------------------------------

    pub fn set_trust_level(&self, level: TrustLevel) {
        self.state.lock().unwrap().trust_level = level;
    }

    pub fn trust_level(&self) -> TrustLevel {
        self.state.lock().unwrap().trust_level.clone()
    }

    /// Check if a replication from `sender` should be auto-accepted.
    pub fn should_auto_accept(&self, sender: &NodeId) -> bool {
        let st = self.state.lock().unwrap();
        match st.trust_level {
            TrustLevel::All => true,
            TrustLevel::Mesh => st.peers.contains_key(sender),
            TrustLevel::Family => {
                st.parent_id.as_ref() == Some(sender) || st.children_ids.contains(sender)
            }
            TrustLevel::None => false,
        }
    }

    /// Queue a replication request for manual approval.
    pub fn queue_request(
        &self,
        sender: NodeId,
        fitness: i64,
        generation: u32,
        size: u64,
        reason: String,
    ) -> u32 {
        let mut st = self.state.lock().unwrap();
        // Rate limit: max 3 pending per peer.
        let from_peer = st
            .pending_requests
            .iter()
            .filter(|r| r.sender_id == sender)
            .count();
        if from_peer >= 3 {
            return 0;
        }
        let id = st.next_request_id;
        st.next_request_id += 1;
        st.pending_requests.push(ReplicationRequest {
            id,
            sender_id: sender,
            sender_fitness: fitness,
            sender_generation: generation,
            package_size: size,
            reason,
            received_at: Instant::now(),
        });
        id
    }

    /// Accept the oldest pending request. Returns sender info if found.
    pub fn accept_oldest(&self) -> Option<(NodeId, u32)> {
        let mut st = self.state.lock().unwrap();
        if st.pending_requests.is_empty() {
            return None;
        }
        let req = st.pending_requests.remove(0);
        self.log_replication(&mut st, "incoming", &req.sender_id, &req.reason, "accepted");
        Some((req.sender_id, req.id))
    }

    /// Deny the oldest pending request.
    pub fn deny_oldest(&self) -> Option<u32> {
        let mut st = self.state.lock().unwrap();
        if st.pending_requests.is_empty() {
            return None;
        }
        let req = st.pending_requests.remove(0);
        self.log_replication(&mut st, "incoming", &req.sender_id, &req.reason, "denied");
        Some(req.id)
    }

    /// Deny all pending requests.
    pub fn deny_all_requests(&self) -> usize {
        let mut st = self.state.lock().unwrap();
        let count = st.pending_requests.len();
        let entries: Vec<ReplicationLogEntry> = st
            .pending_requests
            .iter()
            .map(|req| ReplicationLogEntry {
                timestamp: now_secs(),
                direction: "incoming".into(),
                peer_id: req.sender_id,
                reason: req.reason.clone(),
                result: "denied".into(),
            })
            .collect();
        for entry in entries {
            st.replication_log.push(entry);
        }
        st.pending_requests.clear();
        count
    }

    /// Expire requests older than 60 seconds.
    pub fn expire_requests(&self) {
        let mut st = self.state.lock().unwrap();
        let before = st.pending_requests.len();
        st.pending_requests
            .retain(|r| r.received_at.elapsed().as_secs() < 60);
        let expired = before - st.pending_requests.len();
        if expired > 0 {
            st.log_event(format!("{} replication request(s) expired", expired));
        }
    }

    /// Format pending requests for display.
    pub fn format_requests(&self) -> String {
        let st = self.state.lock().unwrap();
        if st.pending_requests.is_empty() {
            return "  (no pending requests)\n".to_string();
        }
        let mut out = String::new();
        for r in &st.pending_requests {
            let age = r.received_at.elapsed().as_secs();
            out.push_str(&format!(
                "  #{} from {} gen={} fitness={} size={}KB age={}s: {}\n",
                r.id,
                id_to_hex(&r.sender_id),
                r.sender_generation,
                r.sender_fitness,
                r.package_size / 1024,
                age,
                r.reason
            ));
        }
        out
    }

    /// Format replication log.
    pub fn format_replication_log(&self) -> String {
        let st = self.state.lock().unwrap();
        if st.replication_log.is_empty() {
            return "  (no replication history)\n".to_string();
        }
        let mut out = String::new();
        for e in st.replication_log.iter().rev().take(20) {
            out.push_str(&format!(
                "  {} {} {} [{}]: {}\n",
                e.timestamp,
                e.direction,
                id_to_hex(&e.peer_id),
                e.result,
                e.reason
            ));
        }
        out
    }

    fn log_replication(
        &self,
        st: &mut MeshState,
        direction: &str,
        peer: &NodeId,
        reason: &str,
        result: &str,
    ) {
        st.replication_log.push(ReplicationLogEntry {
            timestamp: now_secs(),
            direction: direction.into(),
            peer_id: *peer,
            reason: reason.into(),
            result: result.into(),
        });
        if st.replication_log.len() > 100 {
            st.replication_log.remove(0);
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Network thread
// ---------------------------------------------------------------------------

fn network_thread(socket: UdpSocket, state: Arc<Mutex<MeshState>>, my_id: NodeId) {
    let _ = socket.set_read_timeout(Some(RECV_TIMEOUT));
    let mut recv_buf = [0u8; 65535];
    let mut last_heartbeat = Instant::now() - HEARTBEAT_INTERVAL; // send immediately

    loop {
        // Check if we should stop.
        {
            let st = state.lock().unwrap();
            if !st.running {
                return;
            }
        }

        // Try to receive a packet.
        if let Ok((len, src_addr)) = socket.recv_from(&mut recv_buf) {
            let packet = &recv_buf[..len];
            handle_packet(packet, src_addr, &socket, &state, &my_id);
        }

        // Send heartbeat if interval elapsed.
        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            send_heartbeat(&socket, &state, &my_id);
            last_heartbeat = Instant::now();
        }

        // Check proposal timeouts and commitments.
        check_proposals(&socket, &state, &my_id);

        // Prune stale peers.
        prune_peers(&state);

        // Check if goal load warrants auto-replication.
        check_auto_replication(&state);

        // Send discovery beacon periodically.
        if last_heartbeat.elapsed() >= DISCOVERY_INTERVAL {
            let st = state.lock().unwrap();
            if st.auto_discover {
                let port = st.port;
                drop(st);
                let mut buf = Vec::with_capacity(HEADER_SIZE);
                encode_header(&mut buf, MSG_DISCOVERY_BEACON, &my_id, port);
                let _ = socket.send_to(&buf, format!("127.0.0.1:{}", DISCOVERY_PORT));
            }
        }
    }
}

fn send_heartbeat(socket: &UdpSocket, state: &Arc<Mutex<MeshState>>, my_id: &NodeId) {
    let st = state.lock().unwrap();
    let port = st.port;
    let load = st.load;
    let capacity = st.capacity;
    let peer_count = st.peers.len() as u16;

    // Collect peer addresses for gossip (up to MAX_GOSSIP_PEERS).
    let gossip_addrs: Vec<SocketAddr> = st
        .peers
        .values()
        .take(MAX_GOSSIP_PEERS)
        .map(|p| p.addr)
        .collect();

    let fitness = st.fitness;
    let all_peer_addrs: Vec<SocketAddr> = st.peers.values().map(|p| p.addr).collect();
    drop(st);

    let mut buf = Vec::with_capacity(HEADER_SIZE + 19 + gossip_addrs.len() * 6);
    encode_header(&mut buf, MSG_HEARTBEAT, my_id, port);
    write_u32(&mut buf, load);
    write_u32(&mut buf, capacity);
    write_u16(&mut buf, peer_count);
    write_u8(&mut buf, gossip_addrs.len() as u8);
    for addr in &gossip_addrs {
        if let SocketAddr::V4(v4) = addr {
            write_bytes(&mut buf, &v4.ip().octets());
            write_u16(&mut buf, v4.port());
        }
    }
    // Fitness score appended after gossip addresses.
    write_i64(&mut buf, fitness);

    for addr in &all_peer_addrs {
        let _ = socket.send_to(&buf, addr);
    }
}

fn handle_packet(
    data: &[u8],
    src_addr: SocketAddr,
    socket: &UdpSocket,
    state: &Arc<Mutex<MeshState>>,
    my_id: &NodeId,
) {
    let mut pos = 0;
    let (msg_type, sender_id, sender_port) = match decode_header(data, &mut pos) {
        Some(h) => h,
        None => return, // invalid packet
    };

    // Ignore our own messages.
    if sender_id == *my_id {
        return;
    }

    // Reconstruct the sender's listening address. Use the IP from the UDP
    // source but the port from the header (they may differ if the OS assigns
    // ephemeral source ports, but for our socket-per-node design they match).
    let sender_addr = SocketAddr::new(src_addr.ip(), sender_port);

    match msg_type {
        MSG_HEARTBEAT => {
            handle_heartbeat(data, &mut pos, sender_id, sender_addr, socket, state, my_id)
        }
        MSG_PROPOSE => handle_propose(data, &mut pos, sender_id, sender_addr, socket, state, my_id),
        MSG_VOTE => handle_vote(data, &mut pos, sender_id, socket, state, my_id),
        MSG_COMMIT => handle_commit(data, &mut pos, sender_id, state),
        MSG_REJECT => handle_reject(data, &mut pos, sender_id, state),
        MSG_DATA => handle_data(data, &mut pos, sender_id, state),
        MSG_GOAL_BROADCAST => handle_goal_broadcast(data, &mut pos, sender_id, state),
        MSG_TASK_CLAIM => handle_task_claim(data, &mut pos, sender_id, state),
        MSG_TASK_RESULT => handle_task_result(data, &mut pos, sender_id, state),
        MSG_WORD_SHARE => handle_word_share(data, &mut pos, sender_id, state),
        MSG_DISCOVERY_BEACON => {} // handled by discovery listener
        MSG_SPAWN_INTENT => {
            let mut st = state.lock().unwrap();
            st.spawn_intent_active = true;
            st.log_event(format!("spawn intent from {}", id_to_hex(&sender_id)));
        }
        MSG_CULL_INTENT => {
            let mut st = state.lock().unwrap();
            st.cull_intent_active = true;
            st.log_event(format!("cull intent from {}", id_to_hex(&sender_id)));
        }
        MSG_REPLICATE_REQUEST => {
            handle_replicate_request(data, &mut pos, sender_id, state, socket, my_id);
        }
        MSG_REPLICATE_ACCEPT | MSG_REPLICATE_DENY => {
            // Handled by the sender's send_replicate_request flow.
            let mut st = state.lock().unwrap();
            let result = if msg_type == MSG_REPLICATE_ACCEPT {
                "accepted"
            } else {
                "denied"
            };
            st.log_event(format!(
                "replication {} by {}",
                result,
                id_to_hex(&sender_id)
            ));
        }
        _ => {} // unknown message type — ignore
    }
}

fn handle_heartbeat(
    data: &[u8],
    pos: &mut usize,
    sender_id: NodeId,
    sender_addr: SocketAddr,
    socket: &UdpSocket,
    state: &Arc<Mutex<MeshState>>,
    my_id: &NodeId,
) {
    let load = match read_u32(data, pos) {
        Some(v) => v,
        None => return,
    };
    let capacity = match read_u32(data, pos) {
        Some(v) => v,
        None => return,
    };
    let peer_count = match read_u16(data, pos) {
        Some(v) => v,
        None => return,
    };
    let gossip_count = match read_u8(data, pos) {
        Some(v) => v as usize,
        None => return,
    };

    // Parse gossiped peer addresses.
    let mut gossip_addrs = Vec::with_capacity(gossip_count);
    for _ in 0..gossip_count {
        let ip_bytes = match read_bytes(data, pos, 4) {
            Some(b) => b,
            None => break,
        };
        let port = match read_u16(data, pos) {
            Some(v) => v,
            None => break,
        };
        let ip = Ipv4Addr::new(ip_bytes[0], ip_bytes[1], ip_bytes[2], ip_bytes[3]);
        gossip_addrs.push(SocketAddr::V4(SocketAddrV4::new(ip, port)));
    }

    let mut st = state.lock().unwrap();
    let my_port = st.port;

    // Remove any pseudo-ID entry for this address (from seed peers).
    let pseudo = addr_to_pseudo_id(&sender_addr);
    if pseudo != sender_id {
        st.peers.remove(&pseudo);
    }

    // Update or insert the peer.
    let is_new = !st.peers.contains_key(&sender_id);
    st.peers.insert(
        sender_id,
        PeerInfo {
            addr: sender_addr,
            id: sender_id,
            load,
            capacity,
            peer_count,
            fitness: 0, // updated below from heartbeat data
            last_seen: Instant::now(),
        },
    );

    if is_new {
        st.log_event(format!(
            "discovered peer {} @ {}",
            id_to_hex(&sender_id),
            sender_addr
        ));

        // Broadcast all active goals to the new peer so it converges.
        let port = st.port;
        let active_goals: Vec<(Goal, Vec<Task>)> = st
            .goals
            .goals
            .values()
            .filter(|g| g.status != GoalStatus::Failed)
            .map(|g| {
                let tasks: Vec<Task> = g
                    .task_ids
                    .iter()
                    .filter_map(|tid| st.goals.tasks.get(tid).cloned())
                    .collect();
                (g.clone(), tasks)
            })
            .collect();
        // Send outside the lock via deferred sends.
        for (goal, tasks) in &active_goals {
            let buf = encode_goal_broadcast(my_id, port, goal, tasks);
            let _ = socket.send_to(&buf, sender_addr);
        }
    }

    // Add gossiped peers we don't know about yet (transitive discovery).
    for addr in gossip_addrs {
        // Don't add ourselves.
        if addr.port() == my_port && addr.ip().is_loopback() {
            continue;
        }
        // Check if we already know a peer at this address.
        let known = st.peers.values().any(|p| p.addr == addr);
        if !known {
            let pseudo_id = addr_to_pseudo_id(&addr);
            st.peers.insert(
                pseudo_id,
                PeerInfo {
                    addr,
                    id: pseudo_id,
                    load: 0,
                    capacity: 0,
                    peer_count: 0,
                    fitness: 0,
                    last_seen: Instant::now(),
                },
            );
            st.log_event(format!("gossip: discovered peer at {}", addr));
        }
    }

    // Parse fitness score (appended after gossip, backward-compatible).
    if let Some(fitness) = read_i64(data, pos) {
        if let Some(peer) = st.peers.get_mut(&sender_id) {
            peer.fitness = fitness;
        }
    }
}

fn handle_propose(
    data: &[u8],
    pos: &mut usize,
    sender_id: NodeId,
    sender_addr: SocketAddr,
    socket: &UdpSocket,
    state: &Arc<Mutex<MeshState>>,
    my_id: &NodeId,
) {
    let proposal_id = match read_u64(data, pos) {
        Some(v) => v,
        None => return,
    };
    let reason_len = match read_u16(data, pos) {
        Some(v) => v as usize,
        None => return,
    };
    let reason_bytes = match read_bytes(data, pos, reason_len) {
        Some(v) => v,
        None => return,
    };
    let reason = String::from_utf8_lossy(&reason_bytes).to_string();

    let mut st = state.lock().unwrap();
    st.log_event(format!(
        "proposal #{:016x} from {}: {}",
        proposal_id,
        id_to_hex(&sender_id),
        reason
    ));

    // Evaluate: vote YES if we have available capacity.
    let vote = st.load < (st.capacity * 80 / 100);
    let port = st.port;
    drop(st);

    // Send vote.
    let mut buf = Vec::with_capacity(HEADER_SIZE + 9);
    encode_header(&mut buf, MSG_VOTE, my_id, port);
    write_u64(&mut buf, proposal_id);
    write_u8(&mut buf, if vote { 1 } else { 0 });

    let _ = socket.send_to(&buf, sender_addr);
}

fn handle_vote(
    data: &[u8],
    pos: &mut usize,
    sender_id: NodeId,
    _socket: &UdpSocket,
    state: &Arc<Mutex<MeshState>>,
    _my_id: &NodeId,
) {
    let proposal_id = match read_u64(data, pos) {
        Some(v) => v,
        None => return,
    };
    let vote_val = match read_u8(data, pos) {
        Some(v) => v,
        None => return,
    };

    let mut st = state.lock().unwrap();
    let mut yes_count = 0;
    let mut no_count = 0;
    if let Some(prop) = st.proposals.get_mut(&proposal_id) {
        if vote_val == 1 {
            prop.votes_yes.insert(sender_id);
        } else {
            prop.votes_no.insert(sender_id);
        }
        yes_count = prop.votes_yes.len();
        no_count = prop.votes_no.len();
    }
    st.log_event(format!(
        "vote {} from {} on #{:016x} (yes={}, no={})",
        if vote_val == 1 { "YES" } else { "NO" },
        id_to_hex(&sender_id),
        proposal_id,
        yes_count,
        no_count,
    ));
}

fn handle_commit(data: &[u8], pos: &mut usize, sender_id: NodeId, state: &Arc<Mutex<MeshState>>) {
    let proposal_id = match read_u64(data, pos) {
        Some(v) => v,
        None => return,
    };
    let state_len = match read_u32(data, pos) {
        Some(v) => v as usize,
        None => return,
    };
    let state_bytes = match read_bytes(data, pos, state_len) {
        Some(v) => v,
        None => return,
    };

    let mut st = state.lock().unwrap();
    st.log_event(format!(
        "commit #{:016x} from {} ({} bytes of state)",
        proposal_id,
        id_to_hex(&sender_id),
        state_bytes.len()
    ));

    // Store the received state as an inbox message so the Forth layer can
    // inspect it. In a full implementation, this would bootstrap a new unit.
    st.inbox.push_back(InboxMessage {
        from: sender_id,
        data: state_bytes,
    });
}

fn handle_reject(data: &[u8], pos: &mut usize, sender_id: NodeId, state: &Arc<Mutex<MeshState>>) {
    let proposal_id = match read_u64(data, pos) {
        Some(v) => v,
        None => return,
    };

    let mut st = state.lock().unwrap();
    st.log_event(format!(
        "reject #{:016x} from {}",
        proposal_id,
        id_to_hex(&sender_id)
    ));
    st.proposals.remove(&proposal_id);
}

fn handle_data(data: &[u8], pos: &mut usize, sender_id: NodeId, state: &Arc<Mutex<MeshState>>) {
    let data_len = match read_u16(data, pos) {
        Some(v) => v as usize,
        None => return,
    };
    let payload = match read_bytes(data, pos, data_len) {
        Some(v) => v,
        None => return,
    };

    let mut st = state.lock().unwrap();
    st.inbox.push_back(InboxMessage {
        from: sender_id,
        data: payload,
    });
}

// ---------------------------------------------------------------------------
// Goal message encoding and handling
// ---------------------------------------------------------------------------

/// Encode a full goal broadcast message (goal + all its tasks).
fn encode_goal_broadcast(my_id: &NodeId, port: u16, goal: &Goal, tasks: &[Task]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(256);
    encode_header(&mut buf, MSG_GOAL_BROADCAST, my_id, port);
    write_u64(&mut buf, goal.id);
    write_i64(&mut buf, goal.priority);
    write_u8(&mut buf, goal.status.as_u8());
    write_bytes(&mut buf, &goal.creator);
    write_u64(&mut buf, goal.created_at);
    let desc = goal.description.as_bytes();
    write_u16(&mut buf, desc.len() as u16);
    write_bytes(&mut buf, desc);
    // Code payload.
    if let Some(ref code) = goal.code {
        let cb = code.as_bytes();
        write_u8(&mut buf, 1);
        write_u16(&mut buf, cb.len() as u16);
        write_bytes(&mut buf, cb);
    } else {
        write_u8(&mut buf, 0);
    }
    write_u16(&mut buf, tasks.len() as u16);
    for task in tasks {
        write_u64(&mut buf, task.id);
        write_u8(&mut buf, task.status.as_u8());
        if let Some(ref assignee) = task.assigned_to {
            write_u8(&mut buf, 1);
            write_bytes(&mut buf, assignee);
        } else {
            write_u8(&mut buf, 0);
        }
        let tdesc = task.description.as_bytes();
        write_u16(&mut buf, tdesc.len() as u16);
        write_bytes(&mut buf, tdesc);
        // TaskResult serialization.
        if let Some(ref result) = task.result {
            write_u8(&mut buf, 1);
            write_u8(&mut buf, if result.success { 1 } else { 0 });
            write_u16(&mut buf, result.stack_snapshot.len() as u16);
            for &val in &result.stack_snapshot {
                write_i64(&mut buf, val);
            }
            let ob = result.output.as_bytes();
            write_u16(&mut buf, ob.len() as u16);
            write_bytes(&mut buf, ob);
            let eb = result.error.as_deref().unwrap_or("").as_bytes();
            write_u16(&mut buf, eb.len() as u16);
            write_bytes(&mut buf, eb);
        } else {
            write_u8(&mut buf, 0);
        }
    }
    buf
}

fn handle_goal_broadcast(
    data: &[u8],
    pos: &mut usize,
    sender_id: NodeId,
    state: &Arc<Mutex<MeshState>>,
) {
    let goal_id = match read_u64(data, pos) {
        Some(v) => v,
        None => return,
    };
    let priority = match read_i64(data, pos) {
        Some(v) => v,
        None => return,
    };
    let status = match read_u8(data, pos) {
        Some(v) => GoalStatus::from_u8(v),
        None => return,
    };
    let creator = match read_bytes(data, pos, 8) {
        Some(v) => {
            let mut id = [0u8; 8];
            id.copy_from_slice(&v);
            id
        }
        None => return,
    };
    let created_at = match read_u64(data, pos) {
        Some(v) => v,
        None => return,
    };
    let desc_len = match read_u16(data, pos) {
        Some(v) => v as usize,
        None => return,
    };
    let desc_bytes = match read_bytes(data, pos, desc_len) {
        Some(v) => v,
        None => return,
    };
    let description = String::from_utf8_lossy(&desc_bytes).to_string();

    // Code payload.
    let has_code = match read_u8(data, pos) {
        Some(v) => v != 0,
        None => return,
    };
    let code = if has_code {
        let clen = match read_u16(data, pos) {
            Some(v) => v as usize,
            None => return,
        };
        let cb = match read_bytes(data, pos, clen) {
            Some(v) => v,
            None => return,
        };
        Some(String::from_utf8_lossy(&cb).to_string())
    } else {
        None
    };

    let task_count = match read_u16(data, pos) {
        Some(v) => v as usize,
        None => return,
    };

    let mut task_ids = Vec::with_capacity(task_count);
    let mut tasks = Vec::with_capacity(task_count);
    for _ in 0..task_count {
        let task_id = match read_u64(data, pos) {
            Some(v) => v,
            None => return,
        };
        let task_status = match read_u8(data, pos) {
            Some(v) => TaskStatus::from_u8(v),
            None => return,
        };
        let has_assignee = match read_u8(data, pos) {
            Some(v) => v != 0,
            None => return,
        };
        let assigned_to = if has_assignee {
            match read_bytes(data, pos, 8) {
                Some(v) => {
                    let mut id = [0u8; 8];
                    id.copy_from_slice(&v);
                    Some(id)
                }
                None => return,
            }
        } else {
            None
        };
        let tdesc_len = match read_u16(data, pos) {
            Some(v) => v as usize,
            None => return,
        };
        let tdesc_bytes = match read_bytes(data, pos, tdesc_len) {
            Some(v) => v,
            None => return,
        };
        let task_desc = String::from_utf8_lossy(&tdesc_bytes).to_string();
        let has_result = match read_u8(data, pos) {
            Some(v) => v != 0,
            None => return,
        };
        let result = if has_result {
            let success = match read_u8(data, pos) {
                Some(v) => v != 0,
                None => return,
            };
            let slen = match read_u16(data, pos) {
                Some(v) => v as usize,
                None => return,
            };
            let mut stack_snapshot = Vec::with_capacity(slen);
            for _ in 0..slen {
                match read_i64(data, pos) {
                    Some(v) => stack_snapshot.push(v),
                    None => return,
                }
            }
            let olen = match read_u16(data, pos) {
                Some(v) => v as usize,
                None => return,
            };
            let output = match read_bytes(data, pos, olen) {
                Some(v) => String::from_utf8_lossy(&v).to_string(),
                None => return,
            };
            let elen = match read_u16(data, pos) {
                Some(v) => v as usize,
                None => return,
            };
            let error = if elen > 0 {
                match read_bytes(data, pos, elen) {
                    Some(v) => Some(String::from_utf8_lossy(&v).to_string()),
                    None => return,
                }
            } else {
                None
            };
            Some(TaskResult {
                stack_snapshot,
                output,
                success,
                error,
            })
        } else {
            None
        };

        task_ids.push(task_id);
        tasks.push(Task {
            id: task_id,
            goal_id,
            description: task_desc,
            code: None, // per-task code not transmitted in gossip (uses goal code)
            assigned_to,
            status: task_status,
            result,
            created_at,
        });
    }

    let goal = Goal {
        id: goal_id,
        description,
        code,
        priority,
        status,
        created_at,
        creator,
        task_ids,
    };

    let mut st = state.lock().unwrap();
    let is_new = !st.goals.goals.contains_key(&goal_id);
    st.goals.merge_goal(goal);
    for task in tasks {
        st.goals.merge_task(task);
    }
    if is_new {
        let desc = st
            .goals
            .goals
            .get(&goal_id)
            .map(|g| g.description.clone())
            .unwrap_or_else(|| "?".to_string());
        st.log_event(format!(
            "received goal #{} from {}: {}",
            goal_id,
            id_to_hex(&sender_id),
            desc
        ));
    }
}

fn handle_task_claim(
    data: &[u8],
    pos: &mut usize,
    sender_id: NodeId,
    state: &Arc<Mutex<MeshState>>,
) {
    let task_id = match read_u64(data, pos) {
        Some(v) => v,
        None => return,
    };
    let _goal_id = match read_u64(data, pos) {
        Some(v) => v,
        None => return,
    };

    let mut st = state.lock().unwrap();
    if let Some(task) = st.goals.tasks.get_mut(&task_id) {
        // Accept the claim if the task is still waiting, or if this is
        // a work-steal and the task is only claimed (not running).
        // For simplicity: accept if status <= Running.
        if task.status == TaskStatus::Waiting {
            task.assigned_to = Some(sender_id);
            task.status = TaskStatus::Running;
            // Update parent goal status.
            let gid = task.goal_id;
            if let Some(goal) = st.goals.goals.get_mut(&gid) {
                if goal.status == GoalStatus::Pending {
                    goal.status = GoalStatus::Active;
                }
            }
            st.log_event(format!(
                "peer {} claimed task #{}",
                id_to_hex(&sender_id),
                task_id
            ));
        }
    }
}

fn handle_task_result(
    data: &[u8],
    pos: &mut usize,
    sender_id: NodeId,
    state: &Arc<Mutex<MeshState>>,
) {
    let task_id = match read_u64(data, pos) {
        Some(v) => v,
        None => return,
    };
    let _goal_id = match read_u64(data, pos) {
        Some(v) => v,
        None => return,
    };
    let success = match read_u8(data, pos) {
        Some(v) => v != 0,
        None => return,
    };
    // Decode stack snapshot.
    let slen = match read_u16(data, pos) {
        Some(v) => v as usize,
        None => return,
    };
    let mut stack_snapshot = Vec::with_capacity(slen);
    for _ in 0..slen {
        match read_i64(data, pos) {
            Some(v) => stack_snapshot.push(v),
            None => return,
        }
    }
    // Decode output.
    let olen = match read_u16(data, pos) {
        Some(v) => v as usize,
        None => return,
    };
    let output = match read_bytes(data, pos, olen) {
        Some(v) => String::from_utf8_lossy(&v).to_string(),
        None => return,
    };
    // Decode error.
    let elen = match read_u16(data, pos) {
        Some(v) => v as usize,
        None => return,
    };
    let error = if elen > 0 {
        match read_bytes(data, pos, elen) {
            Some(v) => Some(String::from_utf8_lossy(&v).to_string()),
            None => return,
        }
    } else {
        None
    };

    let result = TaskResult {
        stack_snapshot,
        output,
        success,
        error,
    };

    let mut st = state.lock().unwrap();
    st.goals.complete_task(task_id, Some(result));
    st.log_event(format!(
        "task #{} result from {}",
        task_id,
        id_to_hex(&sender_id)
    ));
}

fn handle_replicate_request(
    data: &[u8],
    pos: &mut usize,
    sender_id: NodeId,
    state: &Arc<Mutex<MeshState>>,
    socket: &UdpSocket,
    my_id: &NodeId,
) {
    let fitness = read_i64(data, pos).unwrap_or(0);
    let generation = read_u32(data, pos).unwrap_or(0);
    let package_size = read_u64(data, pos).unwrap_or(0);
    let reason_len = read_u16(data, pos).unwrap_or(0) as usize;
    let reason = read_bytes(data, pos, reason_len)
        .map(|b| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_default();

    // Reject if > 100MB.
    if package_size > 100_000_000 {
        let mut st = state.lock().unwrap();
        st.log_event(format!(
            "rejected oversized replication from {} ({}MB)",
            id_to_hex(&sender_id),
            package_size / 1_000_000
        ));
        return;
    }

    let mut st = state.lock().unwrap();

    // Check trust level.
    let auto_accept = match st.trust_level {
        TrustLevel::All => true,
        TrustLevel::Mesh => st.peers.contains_key(&sender_id),
        TrustLevel::Family => {
            st.parent_id.as_ref() == Some(&sender_id) || st.children_ids.contains(&sender_id)
        }
        TrustLevel::None => false,
    };

    if auto_accept {
        let trust_label = st.trust_level.label().to_string();
        st.log_event(format!(
            "[auto-accepted from {} (trust: {})]",
            id_to_hex(&sender_id),
            trust_label
        ));
        // Send accept response.
        let port = st.port;
        drop(st);
        let mut buf = Vec::with_capacity(HEADER_SIZE);
        encode_header(&mut buf, MSG_REPLICATE_ACCEPT, my_id, port);
        if let Some(peer) = state.lock().unwrap().peers.get(&sender_id) {
            let _ = socket.send_to(&buf, peer.addr);
        }
    } else {
        // Queue for manual approval.
        let from_peer = st
            .pending_requests
            .iter()
            .filter(|r| r.sender_id == sender_id)
            .count();
        if from_peer >= 3 {
            st.log_event(format!(
                "rate-limited replication from {}",
                id_to_hex(&sender_id)
            ));
            return;
        }
        let rid = st.next_request_id;
        st.next_request_id += 1;
        st.pending_requests.push(ReplicationRequest {
            id: rid,
            sender_id,
            sender_fitness: fitness,
            sender_generation: generation,
            package_size,
            reason: reason.clone(),
            received_at: Instant::now(),
        });
        st.log_event(format!(
            "[replication request #{} from {} (gen {}, fitness {}, {}KB) — ACCEPT or DENY]",
            rid,
            id_to_hex(&sender_id),
            generation,
            fitness,
            package_size / 1024
        ));
    }
}

fn handle_word_share(
    data: &[u8],
    pos: &mut usize,
    sender_id: NodeId,
    state: &Arc<Mutex<MeshState>>,
) {
    let name_len = match read_u16(data, pos) {
        Some(v) => v as usize,
        None => return,
    };
    let name_bytes = match read_bytes(data, pos, name_len) {
        Some(v) => v,
        None => return,
    };
    let name = String::from_utf8_lossy(&name_bytes).to_string();
    let source_len = match read_u16(data, pos) {
        Some(v) => v as usize,
        None => return,
    };
    let source_bytes = match read_bytes(data, pos, source_len) {
        Some(v) => v,
        None => return,
    };
    let source = String::from_utf8_lossy(&source_bytes).to_string();

    let mut st = state.lock().unwrap();
    // Don't re-add if we already have this shared word.
    if st.shared_words.iter().any(|(n, _)| n == &name) {
        return;
    }
    st.word_inbox.push_back(SharedWord {
        name: name.clone(),
        body_source: source,
        origin: sender_id,
    });
    st.shared_words.push((name.clone(), sender_id));
    st.log_event(format!(
        "received word '{}' from {}",
        name,
        id_to_hex(&sender_id)
    ));
}

/// Check if goal load warrants auto-replication.
fn check_auto_replication(state: &Arc<Mutex<MeshState>>) {
    let mut st = state.lock().unwrap();
    if st.auto_replicate_needed {
        return; // already flagged
    }
    let pending = st.goals.pending_task_count();
    let units = st.peers.len() + 1; // self + peers
    if pending > units {
        // Check cooldown.
        if let Some(last) = st.last_proposal_time {
            if last.elapsed() < PROPOSAL_COOLDOWN {
                return;
            }
        }
        // Check no active proposals.
        let has_active = st
            .proposals
            .values()
            .any(|p| !p.committed && p.started.elapsed() < PROPOSAL_TIMEOUT);
        if !has_active {
            st.auto_replicate_needed = true;
            st.log_event(format!(
                "auto-replicate triggered: {} pending tasks, {} units",
                pending, units
            ));
        }
    }
}

/// Check proposals for quorum or timeout, and act accordingly.
fn check_proposals(socket: &UdpSocket, state: &Arc<Mutex<MeshState>>, my_id: &NodeId) {
    let mut st = state.lock().unwrap();
    let port = st.port;

    // Collect proposal IDs to process (avoid borrowing issues).
    let proposal_ids: Vec<u64> = st.proposals.keys().cloned().collect();
    let peer_addrs: Vec<SocketAddr> = st.peers.values().map(|p| p.addr).collect();

    for pid in proposal_ids {
        let prop = match st.proposals.get(&pid) {
            Some(p) => p.clone(),
            None => continue,
        };

        if prop.committed {
            continue;
        }

        // Only the proposer drives the consensus.
        if prop.proposer != *my_id {
            // Clean up foreign proposals after timeout.
            if prop.started.elapsed() > PROPOSAL_TIMEOUT * 2 {
                st.proposals.remove(&pid);
            }
            continue;
        }

        let total = prop.total_peers_at_start;
        let quorum = total / 2 + 1; // >50%

        // Check if quorum reached.
        if prop.votes_yes.len() >= quorum {
            // Quorum reached — commit!
            st.log_event(format!(
                "quorum reached for #{:016x} ({}/{})",
                pid,
                prop.votes_yes.len(),
                total
            ));

            // Mark committed.
            if let Some(p) = st.proposals.get_mut(&pid) {
                p.committed = true;
            }
            st.last_proposal_time = Some(Instant::now());

            // Send COMMIT with serialized state.
            if let Some(state_bytes) = &prop.state_bytes {
                let mut buf = Vec::with_capacity(HEADER_SIZE + 8 + 4 + state_bytes.len());
                encode_header(&mut buf, MSG_COMMIT, my_id, port);
                write_u64(&mut buf, pid);
                write_u32(&mut buf, state_bytes.len() as u32);
                write_bytes(&mut buf, state_bytes);

                for addr in &peer_addrs {
                    let _ = socket.send_to(&buf, addr);
                }
            }

            continue;
        }

        // Check if majority rejected.
        if prop.votes_no.len() > total / 2 {
            st.log_event(format!(
                "proposal #{:016x} rejected ({} NO votes)",
                pid,
                prop.votes_no.len()
            ));
            st.proposals.remove(&pid);
            st.last_proposal_time = Some(Instant::now());

            // Send REJECT to all peers.
            let mut buf = Vec::with_capacity(HEADER_SIZE + 8);
            encode_header(&mut buf, MSG_REJECT, my_id, port);
            write_u64(&mut buf, pid);
            for addr in &peer_addrs {
                let _ = socket.send_to(&buf, addr);
            }
            continue;
        }

        // Check timeout.
        if prop.started.elapsed() > PROPOSAL_TIMEOUT {
            st.log_event(format!(
                "proposal #{:016x} timed out (yes={}, no={}, needed={})",
                pid,
                prop.votes_yes.len(),
                prop.votes_no.len(),
                quorum
            ));
            st.proposals.remove(&pid);
            st.last_proposal_time = Some(Instant::now());

            // Send REJECT.
            let mut buf = Vec::with_capacity(HEADER_SIZE + 8);
            encode_header(&mut buf, MSG_REJECT, my_id, port);
            write_u64(&mut buf, pid);
            for addr in &peer_addrs {
                let _ = socket.send_to(&buf, addr);
            }
        }
    }
}

/// Remove peers that haven't sent a heartbeat within PEER_TIMEOUT.
fn prune_peers(state: &Arc<Mutex<MeshState>>) {
    let mut st = state.lock().unwrap();
    let stale: Vec<NodeId> = st
        .peers
        .iter()
        .filter(|(_, p)| p.last_seen.elapsed() > PEER_TIMEOUT)
        .map(|(id, _)| *id)
        .collect();
    for id in &stale {
        st.peers.remove(id);
        st.log_event(format!("peer {} timed out", id_to_hex(id)));
    }
}

/// Derive a deterministic pseudo-ID from a socket address. Used as a
/// temporary key for seed peers before we learn their real ID.
fn addr_to_pseudo_id(addr: &SocketAddr) -> NodeId {
    let mut id = [0xFFu8; 8];
    let port_bytes = addr.port().to_be_bytes();
    id[0] = port_bytes[0];
    id[1] = port_bytes[1];
    if let SocketAddr::V4(v4) = addr {
        let octets = v4.ip().octets();
        id[2] = octets[0];
        id[3] = octets[1];
        id[4] = octets[2];
        id[5] = octets[3];
    }
    id
}

// ---------------------------------------------------------------------------
// State serialization — dictionary + memory → byte stream
// ---------------------------------------------------------------------------
//
// Wire format:
//   magic: "UNIT" (4 bytes)
//   version: u8 (1)
//   entry_count: u32
//   for each entry:
//     name_len: u16
//     name: [u8; name_len]
//     flags: u8 (bit 0 = immediate, bit 1 = hidden)
//     body_len: u32
//     for each instruction:
//       tag: u8 + payload
//   here: u32
//   memory_cells: u32
//   memory: [i64; memory_cells]

pub fn serialize_state(
    dictionary: &[Entry],
    memory: &[Cell],
    here: usize,
    goals: Option<&GoalRegistry>,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4096);

    // Header.
    write_bytes(&mut buf, MAGIC);
    write_u8(&mut buf, 1); // version

    // Dictionary.
    write_u32(&mut buf, dictionary.len() as u32);
    for entry in dictionary {
        let name_bytes = entry.name.as_bytes();
        write_u16(&mut buf, name_bytes.len() as u16);
        write_bytes(&mut buf, name_bytes);

        let flags = (if entry.immediate { 1u8 } else { 0 }) | (if entry.hidden { 2u8 } else { 0 });
        write_u8(&mut buf, flags);

        write_u32(&mut buf, entry.body.len() as u32);
        for instr in &entry.body {
            serialize_instruction(&mut buf, instr);
        }
    }

    // Memory (only up to `here`).
    let mem_cells = here.min(memory.len());
    write_u32(&mut buf, here as u32);
    write_u32(&mut buf, mem_cells as u32);
    for val in memory.iter().take(mem_cells) {
        write_i64(&mut buf, *val);
    }

    // Goals (appended after memory — backward compatible).
    if let Some(registry) = goals {
        let goal_list: Vec<&Goal> = registry.goals.values().collect();
        write_u32(&mut buf, goal_list.len() as u32);
        for goal in &goal_list {
            write_u64(&mut buf, goal.id);
            write_i64(&mut buf, goal.priority);
            write_u8(&mut buf, goal.status.as_u8());
            write_bytes(&mut buf, &goal.creator);
            write_u64(&mut buf, goal.created_at);
            let desc = goal.description.as_bytes();
            write_u16(&mut buf, desc.len() as u16);
            write_bytes(&mut buf, desc);
            write_u16(&mut buf, goal.task_ids.len() as u16);
            for tid in &goal.task_ids {
                write_u64(&mut buf, *tid);
            }
        }
        let task_list: Vec<&Task> = registry.tasks.values().collect();
        write_u32(&mut buf, task_list.len() as u32);
        for task in &task_list {
            write_u64(&mut buf, task.id);
            write_u64(&mut buf, task.goal_id);
            write_u8(&mut buf, task.status.as_u8());
            if let Some(ref assignee) = task.assigned_to {
                write_u8(&mut buf, 1);
                write_bytes(&mut buf, assignee);
            } else {
                write_u8(&mut buf, 0);
            }
            let tdesc = task.description.as_bytes();
            write_u16(&mut buf, tdesc.len() as u16);
            write_bytes(&mut buf, tdesc);
            if let Some(ref result) = task.result {
                write_u8(&mut buf, 1);
                write_u8(&mut buf, if result.success { 1 } else { 0 });
                write_u16(&mut buf, result.stack_snapshot.len() as u16);
                for &val in &result.stack_snapshot {
                    write_i64(&mut buf, val);
                }
                let ob = result.output.as_bytes();
                write_u16(&mut buf, ob.len() as u16);
                write_bytes(&mut buf, ob);
                let eb = result.error.as_deref().unwrap_or("").as_bytes();
                write_u16(&mut buf, eb.len() as u16);
                write_bytes(&mut buf, eb);
            } else {
                write_u8(&mut buf, 0);
            }
            write_u64(&mut buf, task.created_at);
        }
    }

    buf
}

fn serialize_instruction(buf: &mut Vec<u8>, instr: &Instruction) {
    match instr {
        Instruction::Primitive(id) => {
            write_u8(buf, 0);
            write_u32(buf, *id as u32);
        }
        Instruction::Literal(val) => {
            write_u8(buf, 1);
            write_i64(buf, *val);
        }
        Instruction::Call(idx) => {
            write_u8(buf, 2);
            write_u32(buf, *idx as u32);
        }
        Instruction::StringLit(s) => {
            write_u8(buf, 3);
            let bytes = s.as_bytes();
            write_u32(buf, bytes.len() as u32);
            write_bytes(buf, bytes);
        }
        Instruction::Branch(offset) => {
            write_u8(buf, 4);
            write_i64(buf, *offset);
        }
        Instruction::BranchIfZero(offset) => {
            write_u8(buf, 5);
            write_i64(buf, *offset);
        }
    }
}

pub fn deserialize_state(data: &[u8]) -> Option<(Vec<Entry>, Vec<Cell>, usize)> {
    let mut pos = 0;

    // Header.
    let magic = read_bytes(data, &mut pos, 4)?;
    if magic != MAGIC {
        return None;
    }
    let version = read_u8(data, &mut pos)?;
    if version != 1 {
        return None;
    }

    // Dictionary.
    let entry_count = read_u32(data, &mut pos)? as usize;
    let mut dictionary = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        let name_len = read_u16(data, &mut pos)? as usize;
        let name_bytes = read_bytes(data, &mut pos, name_len)?;
        let name = String::from_utf8_lossy(&name_bytes).to_string();

        let flags = read_u8(data, &mut pos)?;
        let immediate = flags & 1 != 0;
        let hidden = flags & 2 != 0;

        let body_len = read_u32(data, &mut pos)? as usize;
        let mut body = Vec::with_capacity(body_len);
        for _ in 0..body_len {
            let instr = deserialize_instruction(data, &mut pos)?;
            body.push(instr);
        }

        dictionary.push(Entry {
            name,
            immediate,
            hidden,
            body,
        });
    }

    // Memory.
    let here = read_u32(data, &mut pos)? as usize;
    let mem_cells = read_u32(data, &mut pos)? as usize;
    let mut memory = vec![0i64; 65536];
    for i in 0..mem_cells.min(memory.len()) {
        memory[i] = read_i64(data, &mut pos)?;
    }

    Some((dictionary, memory, here))
}

fn deserialize_instruction(data: &[u8], pos: &mut usize) -> Option<Instruction> {
    let tag = read_u8(data, pos)?;
    match tag {
        0 => {
            let id = read_u32(data, pos)? as usize;
            Some(Instruction::Primitive(id))
        }
        1 => {
            let val = read_i64(data, pos)?;
            Some(Instruction::Literal(val))
        }
        2 => {
            let idx = read_u32(data, pos)? as usize;
            Some(Instruction::Call(idx))
        }
        3 => {
            let len = read_u32(data, pos)? as usize;
            let bytes = read_bytes(data, pos, len)?;
            let s = String::from_utf8_lossy(&bytes).to_string();
            Some(Instruction::StringLit(s))
        }
        4 => {
            let offset = read_i64(data, pos)?;
            Some(Instruction::Branch(offset))
        }
        5 => {
            let offset = read_i64(data, pos)?;
            Some(Instruction::BranchIfZero(offset))
        }
        _ => None,
    }
}
