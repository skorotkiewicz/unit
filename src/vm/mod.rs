// vm/mod.rs — The Forth virtual machine
//
// This is the core nanobot: stacks, dictionary, memory, and the inner
// interpreter. Everything else (mesh, goals, features) builds on top.

pub mod compiler;
pub mod primitives;

#[cfg(test)]
mod tests;

use crate::types::{Cell, Entry, Instruction};
use std::collections::{HashSet, VecDeque};
use std::io::{self, Write};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Primitive IDs — assigned in registration order
// ---------------------------------------------------------------------------

pub(crate) const P_DUP: usize = 0;
pub(crate) const P_DROP: usize = 1;
pub(crate) const P_SWAP: usize = 2;
pub(crate) const P_OVER: usize = 3;
pub(crate) const P_ROT: usize = 4;
pub(crate) const P_FETCH: usize = 5;
pub(crate) const P_STORE: usize = 6;
pub(crate) const P_ADD: usize = 7;
pub(crate) const P_SUB: usize = 8;
pub(crate) const P_MUL: usize = 9;
pub(crate) const P_DIV: usize = 10;
pub(crate) const P_MOD: usize = 11;
pub(crate) const P_EQ: usize = 12;
pub(crate) const P_LT: usize = 13;
pub(crate) const P_GT: usize = 14;
pub(crate) const P_AND: usize = 15;
pub(crate) const P_OR: usize = 16;
pub(crate) const P_NOT: usize = 17;
pub(crate) const P_EMIT: usize = 18;
pub(crate) const P_KEY: usize = 19;
pub(crate) const P_CR: usize = 20;
pub(crate) const P_DOT: usize = 21;
pub(crate) const P_DOT_S: usize = 22;
pub(crate) const P_COLON: usize = 23;
pub(crate) const P_SEMICOLON: usize = 24;
pub(crate) const P_CREATE: usize = 25;
pub(crate) const P_DOES: usize = 26;
pub(crate) const P_VARIABLE: usize = 27;
pub(crate) const P_CONSTANT: usize = 28;
pub(crate) const P_IF: usize = 29;
pub(crate) const P_ELSE: usize = 30;
pub(crate) const P_THEN: usize = 31;
pub(crate) const P_DO: usize = 32;
pub(crate) const P_LOOP: usize = 33;
pub(crate) const P_BEGIN: usize = 34;
pub(crate) const P_UNTIL: usize = 35;
pub(crate) const P_WHILE: usize = 36;
pub(crate) const P_REPEAT: usize = 37;
pub(crate) const P_DOT_QUOTE: usize = 38;
pub(crate) const P_PAREN: usize = 39;
pub(crate) const P_BACKSLASH: usize = 40;
pub(crate) const P_WORDS: usize = 41;
pub(crate) const P_SEE: usize = 42;
pub(crate) const P_QUIT: usize = 43;
pub(crate) const P_BYE: usize = 44;
pub(crate) const P_SEND: usize = 45;
pub(crate) const P_RECV: usize = 46;
pub(crate) const P_PEERS: usize = 47;
pub(crate) const P_REPLICATE: usize = 48;
pub(crate) const P_MUTATE: usize = 49;
pub(crate) const P_RECURSE: usize = 50;
pub(crate) const P_MESH_STATUS: usize = 51;
pub(crate) const P_PROPOSE: usize = 52;
pub(crate) const P_LOAD: usize = 53;
pub(crate) const P_CAPACITY: usize = 54;
pub(crate) const P_ID: usize = 55;
pub(crate) const P_TYPE: usize = 56;
pub(crate) const P_GOAL: usize = 57;
pub(crate) const P_GOALS: usize = 58;
pub(crate) const P_TASKS: usize = 59;
pub(crate) const P_TASK_STATUS: usize = 60;
pub(crate) const P_CANCEL: usize = 61;
pub(crate) const P_STEER: usize = 62;
pub(crate) const P_REPORT: usize = 63;
pub(crate) const P_CLAIM: usize = 64;
pub(crate) const P_COMPLETE: usize = 65;
pub(crate) const P_GOAL_EXEC: usize = 66;
pub(crate) const P_EVAL: usize = 67;
pub(crate) const P_RESULT: usize = 68;
pub(crate) const P_AUTO_CLAIM: usize = 69;
pub(crate) const P_TIMEOUT: usize = 70;
pub(crate) const P_GOAL_RESULT: usize = 71;
// Host I/O (immediate — parse string at compile time)
pub(crate) const P_FILE_READ: usize = 72;
pub(crate) const P_FILE_WRITE: usize = 73;
pub(crate) const P_FILE_EXISTS: usize = 74;
pub(crate) const P_FILE_LIST: usize = 75;
pub(crate) const P_FILE_DELETE: usize = 76;
pub(crate) const P_HTTP_GET: usize = 77;
pub(crate) const P_HTTP_POST: usize = 78;
pub(crate) const P_SHELL: usize = 79;
pub(crate) const P_ENV: usize = 80;
// Host I/O (non-immediate)
pub(crate) const P_TIMESTAMP: usize = 81;
pub(crate) const P_SLEEP: usize = 82;
pub(crate) const P_SANDBOX_ON: usize = 83;
pub(crate) const P_SANDBOX_OFF: usize = 84;
pub(crate) const P_IO_LOG: usize = 85;
// Mutation
pub(crate) const P_MUTATE_RAND: usize = 86;
pub(crate) const P_MUTATE_WORD: usize = 87;
pub(crate) const P_UNDO_MUTATE: usize = 88;
pub(crate) const P_MUTATIONS: usize = 89;
// Fitness / Evolution
pub(crate) const P_FITNESS: usize = 90;
pub(crate) const P_LEADERBOARD: usize = 91;
pub(crate) const P_RATE: usize = 92;
pub(crate) const P_EVOLVE: usize = 93;
pub(crate) const P_AUTO_EVOLVE: usize = 94;
pub(crate) const P_BENCHMARK: usize = 95;
// Trust / Security
pub(crate) const P_TRUST: usize = 96;
pub(crate) const P_TRUST_ALL: usize = 97;
pub(crate) const P_TRUST_NONE: usize = 98;
pub(crate) const P_SHELL_ENABLE: usize = 99;
// Identity
pub(crate) const P_REIDENTIFY: usize = 199;
// Persistence
pub(crate) const P_SAVE: usize = 200;
pub(crate) const P_LOAD_STATE: usize = 201;
pub(crate) const P_AUTO_SAVE: usize = 202;
pub(crate) const P_RESET: usize = 203;
pub(crate) const P_SNAPSHOTS: usize = 204;
pub(crate) const P_SNAPSHOT: usize = 205;
pub(crate) const P_RESTORE: usize = 206;
// Loop index
pub(crate) const P_I: usize = 215;
pub(crate) const P_J: usize = 216;
// Spawn / Replication
pub(crate) const P_SPAWN: usize = 220;
pub(crate) const P_SPAWN_N: usize = 221;
pub(crate) const P_PACKAGE: usize = 222;
pub(crate) const P_PACKAGE_SIZE: usize = 223;
pub(crate) const P_CHILDREN: usize = 224;
pub(crate) const P_FAMILY: usize = 225;
pub(crate) const P_GENERATION: usize = 226;
pub(crate) const P_KILL_CHILD: usize = 227;
pub(crate) const P_REPLICATE_TO: usize = 228;
pub(crate) const P_ACCEPT_REPL: usize = 229;
pub(crate) const P_DENY_REPL: usize = 230;
pub(crate) const P_QUARANTINE: usize = 231;
pub(crate) const P_MAX_CHILDREN: usize = 232;
// Task decomposition
pub(crate) const P_SUBTASK: usize = 210;
pub(crate) const P_FORK: usize = 211;
pub(crate) const P_RESULTS: usize = 212;
pub(crate) const P_REDUCE: usize = 213;
pub(crate) const P_PROGRESS: usize = 214;
// Monitoring & Ops
pub(crate) const P_WATCH_URL: usize = 300;
pub(crate) const P_WATCH_FILE: usize = 301;
pub(crate) const P_WATCH_PROC: usize = 302;
pub(crate) const P_WATCHES: usize = 303;
pub(crate) const P_UNWATCH: usize = 304;
pub(crate) const P_WATCH_LOG: usize = 305;
pub(crate) const P_ON_ALERT: usize = 306;
pub(crate) const P_ALERTS: usize = 307;
pub(crate) const P_ACK: usize = 308;
pub(crate) const P_ALERT_HISTORY: usize = 309;
pub(crate) const P_DASHBOARD: usize = 310;
pub(crate) const P_HEALTH: usize = 311;
pub(crate) const P_UPTIME: usize = 312;
pub(crate) const P_EVERY: usize = 313;
pub(crate) const P_SCHEDULE: usize = 314;
pub(crate) const P_UNSCHED: usize = 315;
pub(crate) const P_HEAL: usize = 316;
pub(crate) const P_HEALTH_PORT: usize = 317;
pub(crate) const P_ALERT_THRESHOLD: usize = 318;
// WebSocket bridge
pub(crate) const P_WS_STATUS: usize = 320;
pub(crate) const P_WS_CLIENTS: usize = 321;
pub(crate) const P_WS_PORT: usize = 322;
pub(crate) const P_WS_BROADCAST: usize = 323;
// Swarm
pub(crate) const P_DISCOVER: usize = 330;
pub(crate) const P_AUTO_DISCOVER: usize = 331;
pub(crate) const P_SHARE_WORD: usize = 332;
pub(crate) const P_SHARE_ALL: usize = 333;
pub(crate) const P_AUTO_SHARE: usize = 334;
pub(crate) const P_SHARED_WORDS: usize = 335;
pub(crate) const P_AUTO_SPAWN_TOGGLE: usize = 336;
pub(crate) const P_AUTO_CULL_TOGGLE: usize = 337;
pub(crate) const P_MIN_UNITS: usize = 338;
pub(crate) const P_MAX_UNITS: usize = 339;
pub(crate) const P_SWARM_STATUS: usize = 340;
// Replication consent
pub(crate) const P_TRUST_ALL_LEVEL: usize = 350;
pub(crate) const P_TRUST_MESH: usize = 351;
pub(crate) const P_TRUST_FAMILY: usize = 352;
pub(crate) const P_TRUST_NONE_LEVEL: usize = 353;
pub(crate) const P_TRUST_LEVEL: usize = 354;
pub(crate) const P_REQUESTS: usize = 355;
pub(crate) const P_ACCEPT_REQ: usize = 356;
pub(crate) const P_DENY_REQ: usize = 357;
pub(crate) const P_DENY_ALL_REQ: usize = 358;
pub(crate) const P_REPLICATION_LOG: usize = 359;
// Atom primitives (raw data for Forth-level orchestration)
pub(crate) const P_GOAL_COUNT: usize = 400;
pub(crate) const P_TASK_COUNT: usize = 401;
pub(crate) const P_WATCH_COUNT: usize = 402;
pub(crate) const P_ALERT_COUNT: usize = 403;
pub(crate) const P_CHILD_COUNT: usize = 404;
pub(crate) const P_MESH_AVG_FITNESS: usize = 405;
pub(crate) const P_CHECK_WATCHES: usize = 406;
pub(crate) const P_RUN_HANDLERS: usize = 407;
pub(crate) const P_RUN_BENCHMARK: usize = 408;
pub(crate) const P_MUTATE_RANDOM: usize = 409;
pub(crate) const P_UNDO_LAST_MUTATION: usize = 410;
pub(crate) const P_PEER_COUNT: usize = 411;
pub(crate) const P_SMART_MUTATE: usize = 412;
pub(crate) const P_MUTATION_REPORT: usize = 413;
pub(crate) const P_MUTATION_STATS: usize = 414;
// Internal runtime primitives (not directly user-visible).
pub(crate) const P_DO_RT: usize = 100;
pub(crate) const P_LOOP_RT: usize = 101;
pub(crate) const P_GOAL_EXEC_RT: usize = 102;
pub(crate) const P_IO_RT: usize = 103;
pub(crate) const P_MUTATE_WORD_RT: usize = 104;
pub(crate) const P_BENCHMARK_RT: usize = 105;
pub(crate) const P_REDUCE_RT: usize = 106;
pub(crate) const P_WATCH_URL_RT: usize = 107;
pub(crate) const P_WATCH_FILE_RT: usize = 108;
pub(crate) const P_WATCH_PROC_RT: usize = 109;
pub(crate) const P_ON_ALERT_RT: usize = 110;
pub(crate) const P_EVERY_RT: usize = 111;
pub(crate) const P_ALERT_THRESHOLD_RT: usize = 112;
// VM: the Forth virtual machine
// ---------------------------------------------------------------------------

// PAD is imported from types.rs

pub struct VM {
    pub stack: Vec<Cell>,
    pub rstack: Vec<Cell>,
    pub dictionary: Vec<Entry>,
    pub memory: Vec<Cell>,
    /// Next free address in the memory heap (bump allocator).
    pub here: usize,
    pub primitive_names: Vec<(String, usize)>,
    pub compiling: bool,
    pub current_def: Option<Entry>,
    pub input_buffer: String,
    pub input_pos: usize,
    pub running: bool,
    pub silent: bool,
    /// Mesh networking node (None if offline).
    pub mesh: Option<crate::mesh::MeshNode>,
    /// When set, output goes here instead of stdout (sandbox mode).
    pub output_buffer: Option<String>,
    /// Execution deadline for sandboxed task execution.
    pub deadline: Option<Instant>,
    /// Set when execution exceeds the deadline.
    pub timed_out: bool,
    /// Configurable execution timeout in seconds.
    pub execution_timeout: u64,
    /// When true, automatically claim and execute incoming tasks.
    pub auto_claim: bool,
    /// Stored code strings for compiled GOAL{ ... } (indexed by Literal).
    pub code_strings: Vec<String>,
    // --- Sandbox / Security ---
    pub sandbox_active: bool,
    pub shell_enabled: bool,
    pub trusted_peers: HashSet<[u8; 8]>,
    pub io_log: VecDeque<String>,
    // --- Mutation ---
    pub mutation_history: Vec<crate::features::mutation::MutationRecord>,
    pub mutation_stats: crate::features::mutation::MutationStats,
    pub last_mutation_result: Option<crate::features::mutation::SmartMutationResult>,
    pub rng: crate::features::mutation::SimpleRng,
    // --- Fitness ---
    pub fitness: crate::features::fitness::FitnessTracker,
    // --- Spawn ---
    pub spawn_state: crate::spawn::SpawnState,
    // --- Monitoring ---
    pub monitor: crate::features::monitor::MonitorState,
    // --- WebSocket bridge ---
    pub ws_state:
        Option<std::sync::Arc<std::sync::Mutex<crate::features::ws_bridge::WsBridgeState>>>,
    pub ws_events: Option<std::sync::mpsc::Receiver<crate::features::ws_bridge::WsEvent>>,
    pub ws_mesh_json: std::sync::Arc<std::sync::Mutex<String>>,
    // --- Anonymous definition nesting depth (for interpret-mode control flow) ---
    pub anon_depth: i32,
    // --- Persistence ---
    pub auto_save_enabled: bool,
    pub auto_save_interval: u32,
    pub tasks_since_save: u32,
    pub node_id_cache: Option<[u8; 8]>,
}

impl Default for VM {
    fn default() -> Self {
        Self::new()
    }
}

impl VM {
    pub fn new() -> Self {
        let mut vm = VM {
            stack: Vec::with_capacity(256),
            rstack: Vec::with_capacity(256),
            dictionary: Vec::new(),
            memory: vec![0; 65536],
            here: 1, // address 0 reserved
            primitive_names: Vec::new(),
            compiling: false,
            current_def: None,
            input_buffer: String::new(),
            input_pos: 0,
            running: true,
            silent: false,
            mesh: None,
            output_buffer: None,
            deadline: None,
            timed_out: false,
            execution_timeout: 10,
            auto_claim: false,
            code_strings: Vec::new(),
            sandbox_active: false,
            shell_enabled: false,
            trusted_peers: HashSet::new(),
            io_log: VecDeque::new(),
            mutation_history: Vec::new(),
            mutation_stats: crate::features::mutation::MutationStats::default(),
            last_mutation_result: None,
            rng: crate::features::mutation::SimpleRng::new(0), // re-seeded from node ID in main()
            fitness: crate::features::fitness::FitnessTracker::new(),
            spawn_state: crate::spawn::SpawnState::new(),
            monitor: crate::features::monitor::MonitorState::new(),
            ws_state: None,
            ws_events: None,
            ws_mesh_json: std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            anon_depth: 0,
            auto_save_enabled: false,
            auto_save_interval: 5,
            tasks_since_save: 0,
            node_id_cache: None,
        };
        vm.register_primitives();
        vm
    }

    // -----------------------------------------------------------------------
    // Primitive registration
    // -----------------------------------------------------------------------
    pub(crate) fn register_primitives(&mut self) {
        let prims: &[(&str, usize, bool)] = &[
            ("DUP", P_DUP, false),
            ("DROP", P_DROP, false),
            ("SWAP", P_SWAP, false),
            ("OVER", P_OVER, false),
            ("ROT", P_ROT, false),
            ("@", P_FETCH, false),
            ("!", P_STORE, false),
            ("+", P_ADD, false),
            ("-", P_SUB, false),
            ("*", P_MUL, false),
            ("/", P_DIV, false),
            ("MOD", P_MOD, false),
            ("=", P_EQ, false),
            ("<", P_LT, false),
            (">", P_GT, false),
            ("AND", P_AND, false),
            ("OR", P_OR, false),
            ("NOT", P_NOT, false),
            ("EMIT", P_EMIT, false),
            ("KEY", P_KEY, false),
            ("CR", P_CR, false),
            (".", P_DOT, false),
            (".S", P_DOT_S, false),
            (":", P_COLON, false),
            (";", P_SEMICOLON, true),
            ("CREATE", P_CREATE, false),
            ("DOES>", P_DOES, true),
            ("VARIABLE", P_VARIABLE, false),
            ("CONSTANT", P_CONSTANT, false),
            ("IF", P_IF, true),
            ("ELSE", P_ELSE, true),
            ("THEN", P_THEN, true),
            ("DO", P_DO, true),
            ("LOOP", P_LOOP, true),
            ("BEGIN", P_BEGIN, true),
            ("UNTIL", P_UNTIL, true),
            ("WHILE", P_WHILE, true),
            ("REPEAT", P_REPEAT, true),
            (".\"", P_DOT_QUOTE, true),
            ("(", P_PAREN, true),
            ("\\", P_BACKSLASH, true),
            ("WORDS", P_WORDS, false),
            ("SEE", P_SEE, false),
            ("QUIT", P_QUIT, false),
            ("BYE", P_BYE, false),
            ("SEND", P_SEND, false),
            ("RECV", P_RECV, false),
            ("PEERS", P_PEERS, false),
            ("REPLICATE", P_REPLICATE, false),
            ("MUTATE", P_MUTATE, false),
            ("RECURSE", P_RECURSE, true),
            ("MESH-STATUS", P_MESH_STATUS, false),
            ("PROPOSE", P_PROPOSE, false),
            ("LOAD", P_LOAD, false),
            ("CAPACITY", P_CAPACITY, false),
            ("ID", P_ID, false),
            ("TYPE", P_TYPE, false),
            ("GOAL\"", P_GOAL, true),
            ("GOALS", P_GOALS, false),
            ("TASKS", P_TASKS, false),
            ("TASK-STATUS", P_TASK_STATUS, false),
            ("CANCEL", P_CANCEL, false),
            ("STEER", P_STEER, false),
            ("REPORT", P_REPORT, false),
            ("CLAIM", P_CLAIM, false),
            ("COMPLETE", P_COMPLETE, false),
            ("GOAL{", P_GOAL_EXEC, true),
            ("EVAL\"", P_EVAL, true),
            ("RESULT", P_RESULT, false),
            ("AUTO-CLAIM", P_AUTO_CLAIM, false),
            ("TIMEOUT", P_TIMEOUT, false),
            ("GOAL-RESULT", P_GOAL_RESULT, false),
            // Host I/O
            ("FILE-READ\"", P_FILE_READ, true),
            ("FILE-WRITE\"", P_FILE_WRITE, true),
            ("FILE-EXISTS\"", P_FILE_EXISTS, true),
            ("FILE-LIST\"", P_FILE_LIST, true),
            ("FILE-DELETE\"", P_FILE_DELETE, true),
            ("HTTP-GET\"", P_HTTP_GET, true),
            ("HTTP-POST\"", P_HTTP_POST, true),
            ("SHELL\"", P_SHELL, true),
            ("ENV\"", P_ENV, true),
            ("TIMESTAMP", P_TIMESTAMP, false),
            ("SLEEP", P_SLEEP, false),
            ("SANDBOX-ON", P_SANDBOX_ON, false),
            ("SANDBOX-OFF", P_SANDBOX_OFF, false),
            ("IO-LOG", P_IO_LOG, false),
            // Mutation
            ("MUTATE", P_MUTATE_RAND, false),
            ("MUTATE-WORD\"", P_MUTATE_WORD, true),
            ("UNDO-MUTATE", P_UNDO_MUTATE, false),
            ("MUTATIONS", P_MUTATIONS, false),
            // Fitness / Evolution
            ("FITNESS", P_FITNESS, false),
            ("LEADERBOARD", P_LEADERBOARD, false),
            ("RATE", P_RATE, false),
            ("EVOLVE", P_EVOLVE, false),
            ("AUTO-EVOLVE", P_AUTO_EVOLVE, false),
            ("BENCHMARK\"", P_BENCHMARK, true),
            // Trust / Security
            ("TRUST", P_TRUST, false),
            ("TRUST-ALL", P_TRUST_ALL, false),
            ("TRUST-NONE", P_TRUST_NONE, false),
            ("SHELL-ENABLE", P_SHELL_ENABLE, false),
            // Identity
            ("REIDENTIFY", P_REIDENTIFY, false),
            // Persistence
            ("SAVE", P_SAVE, false),
            ("LOAD-STATE", P_LOAD_STATE, false),
            ("AUTO-SAVE", P_AUTO_SAVE, false),
            ("RESET", P_RESET, false),
            ("SNAPSHOTS", P_SNAPSHOTS, false),
            ("SNAPSHOT", P_SNAPSHOT, false),
            ("RESTORE", P_RESTORE, false),
            // Loop index
            ("I", P_I, false),
            ("J", P_J, false),
            // Spawn / Replication
            ("SPAWN", P_SPAWN, false),
            ("SPAWN-N", P_SPAWN_N, false),
            ("PACKAGE", P_PACKAGE, false),
            ("PACKAGE-SIZE", P_PACKAGE_SIZE, false),
            ("CHILDREN", P_CHILDREN, false),
            ("FAMILY", P_FAMILY, false),
            ("GENERATION", P_GENERATION, false),
            ("KILL-CHILD", P_KILL_CHILD, false),
            ("REPLICATE-TO\"", P_REPLICATE_TO, true),
            ("ACCEPT-REPLICATE", P_ACCEPT_REPL, false),
            ("DENY-REPLICATE", P_DENY_REPL, false),
            ("QUARANTINE", P_QUARANTINE, false),
            ("MAX-CHILDREN", P_MAX_CHILDREN, false),
            // Task decomposition
            // Monitoring & Ops
            ("WATCH\"", P_WATCH_URL, true),
            ("WATCH-FILE\"", P_WATCH_FILE, true),
            ("WATCH-PROC\"", P_WATCH_PROC, true),
            ("WATCHES", P_WATCHES, false),
            ("UNWATCH", P_UNWATCH, false),
            ("WATCH-LOG", P_WATCH_LOG, false),
            ("ON-ALERT\"", P_ON_ALERT, true),
            ("ALERTS", P_ALERTS, false),
            ("ACK", P_ACK, false),
            ("ALERT-HISTORY", P_ALERT_HISTORY, false),
            ("DASHBOARD", P_DASHBOARD, false),
            ("HEALTH", P_HEALTH, false),
            ("UPTIME", P_UPTIME, false),
            ("EVERY", P_EVERY, false),
            ("SCHEDULE", P_SCHEDULE, false),
            ("UNSCHED", P_UNSCHED, false),
            ("HEAL", P_HEAL, false),
            ("HEALTH-PORT", P_HEALTH_PORT, false),
            ("ALERT-THRESHOLD", P_ALERT_THRESHOLD, true),
            // WebSocket bridge
            ("WS-STATUS", P_WS_STATUS, false),
            ("WS-CLIENTS", P_WS_CLIENTS, false),
            ("WS-PORT", P_WS_PORT, false),
            ("WS-BROADCAST\"", P_WS_BROADCAST, true),
            // Swarm
            ("DISCOVER", P_DISCOVER, false),
            ("AUTO-DISCOVER", P_AUTO_DISCOVER, false),
            ("SHARE\"", P_SHARE_WORD, true),
            ("SHARE-ALL", P_SHARE_ALL, false),
            ("AUTO-SHARE", P_AUTO_SHARE, false),
            ("SHARED-WORDS", P_SHARED_WORDS, false),
            ("AUTO-SPAWN", P_AUTO_SPAWN_TOGGLE, false),
            ("AUTO-CULL", P_AUTO_CULL_TOGGLE, false),
            ("MIN-UNITS", P_MIN_UNITS, false),
            ("MAX-UNITS", P_MAX_UNITS, false),
            ("SWARM-STATUS", P_SWARM_STATUS, false),
            // Replication consent
            ("TRUST-ALL", P_TRUST_ALL_LEVEL, false),
            ("TRUST-MESH", P_TRUST_MESH, false),
            ("TRUST-FAMILY", P_TRUST_FAMILY, false),
            ("TRUST-NONE", P_TRUST_NONE_LEVEL, false),
            ("TRUST-LEVEL", P_TRUST_LEVEL, false),
            ("REQUESTS", P_REQUESTS, false),
            ("ACCEPT", P_ACCEPT_REQ, false),
            ("DENY", P_DENY_REQ, false),
            ("DENY-ALL", P_DENY_ALL_REQ, false),
            ("REPLICATION-LOG", P_REPLICATION_LOG, false),
            // Atom primitives (raw data for Forth orchestration)
            ("GOAL-COUNT", P_GOAL_COUNT, false),
            ("TASK-COUNT", P_TASK_COUNT, false),
            ("WATCH-COUNT", P_WATCH_COUNT, false),
            ("ALERT-COUNT", P_ALERT_COUNT, false),
            ("CHILD-COUNT", P_CHILD_COUNT, false),
            ("MESH-AVG-FITNESS", P_MESH_AVG_FITNESS, false),
            ("CHECK-WATCHES", P_CHECK_WATCHES, false),
            ("RUN-HANDLERS", P_RUN_HANDLERS, false),
            ("RUN-BENCHMARK", P_RUN_BENCHMARK, false),
            ("MUTATE-RANDOM", P_MUTATE_RANDOM, false),
            ("UNDO-LAST-MUTATION", P_UNDO_LAST_MUTATION, false),
            ("PEER-COUNT", P_PEER_COUNT, false),
            ("SMART-MUTATE", P_SMART_MUTATE, false),
            ("MUTATION-REPORT", P_MUTATION_REPORT, false),
            ("MUTATION-STATS", P_MUTATION_STATS, false),
            // Task decomposition
            ("SUBTASK{", P_SUBTASK, true),
            ("FORK", P_FORK, false),
            ("RESULTS", P_RESULTS, false),
            ("REDUCE\"", P_REDUCE, true),
            ("PROGRESS", P_PROGRESS, false),
        ];

        for &(name, id, immediate) in prims {
            self.primitive_names.push((name.to_string(), id));
            self.dictionary.push(Entry {
                name: name.to_string(),
                immediate,
                hidden: false,
                body: vec![Instruction::Primitive(id)],
            });
        }
    }
    // -----------------------------------------------------------------------
    // Parser
    // -----------------------------------------------------------------------

    pub fn next_word(&mut self) -> Option<String> {
        let bytes = self.input_buffer.as_bytes();
        while self.input_pos < bytes.len() && (bytes[self.input_pos] as char).is_ascii_whitespace()
        {
            self.input_pos += 1;
        }
        if self.input_pos >= bytes.len() {
            return None;
        }
        let start = self.input_pos;
        while self.input_pos < bytes.len() && !(bytes[self.input_pos] as char).is_ascii_whitespace()
        {
            self.input_pos += 1;
        }
        Some(self.input_buffer[start..self.input_pos].to_string())
    }

    /// Read until a delimiter character (for comments and strings).
    pub fn parse_until(&mut self, delim: char) -> String {
        let bytes = self.input_buffer.as_bytes();
        // Skip one leading space if present.
        if self.input_pos < bytes.len() && bytes[self.input_pos] == b' ' {
            self.input_pos += 1;
        }
        let start = self.input_pos;
        while self.input_pos < bytes.len() && bytes[self.input_pos] as char != delim {
            self.input_pos += 1;
        }
        let result = self.input_buffer[start..self.input_pos].to_string();
        if self.input_pos < bytes.len() {
            self.input_pos += 1; // skip delimiter
        }
        result
    }

    // -----------------------------------------------------------------------
    // Dictionary lookup (search from end — most recent definition wins)
    // -----------------------------------------------------------------------

    pub fn find_word(&self, name: &str) -> Option<usize> {
        let upper = name.to_uppercase();
        self.dictionary
            .iter()
            .rposition(|e| !e.hidden && e.name == upper)
    }

    // -----------------------------------------------------------------------
    // Outer interpreter
    // -----------------------------------------------------------------------

    pub fn interpret_line(&mut self, line: &str) {
        self.input_buffer = line.to_string();
        self.input_pos = 0;

        while let Some(word) = self.next_word() {
            if !self.running {
                return;
            }
            let upper = word.to_uppercase();

            if self.compiling {
                self.compile_word(&upper);
            } else {
                self.interpret_word(&upper);
            }
        }
    }

    pub(crate) fn interpret_word(&mut self, word: &str) {
        if let Some(idx) = self.find_word(word) {
            self.execute_word(idx);
            return;
        }
        if let Ok(n) = word.parse::<Cell>() {
            self.stack.push(n);
            return;
        }
        if !self.silent {
            self.emit_str(&format!("error: unknown word '{}'\n", word));
        }
    }

    pub(crate) fn compile_word(&mut self, word: &str) {
        if let Some(idx) = self.find_word(word) {
            if self.dictionary[idx].immediate {
                self.execute_word(idx);
            } else if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::Call(idx));
            }
            return;
        }
        if let Ok(n) = word.parse::<Cell>() {
            if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::Literal(n));
            }
            return;
        }
        self.emit_str(&format!("error: unknown word '{}'\n", word));
        self.compiling = false;
        self.current_def = None;
    }

    // -----------------------------------------------------------------------
    // Execution engine
    // -----------------------------------------------------------------------

    pub(crate) fn execute_word(&mut self, dict_idx: usize) {
        let body = self.dictionary[dict_idx].body.clone();
        self.execute_body(&body);
    }

    pub(crate) fn execute_body(&mut self, body: &[Instruction]) {
        let mut ip: usize = 0;
        while ip < body.len() {
            // Check for timeout (sandbox execution).
            if self.timed_out {
                return;
            }
            if let Some(deadline) = self.deadline {
                if Instant::now() > deadline {
                    self.timed_out = true;
                    return;
                }
            }

            match &body[ip] {
                Instruction::Primitive(id) => match *id {
                    P_DO_RT => self.rt_do(),
                    P_LOOP_RT => self.rt_loop(),
                    P_GOAL_EXEC_RT => self.rt_goal_exec(),
                    P_IO_RT => self.rt_io(),
                    P_MUTATE_WORD_RT => self.rt_mutate_word(),
                    P_BENCHMARK_RT => self.rt_benchmark(),
                    P_REDUCE_RT => self.rt_reduce(),
                    P_WATCH_URL_RT => self.rt_watch(0),
                    P_WATCH_FILE_RT => self.rt_watch(1),
                    P_WATCH_PROC_RT => self.rt_watch(2),
                    P_ON_ALERT_RT => self.rt_on_alert(),
                    P_EVERY_RT => self.rt_every(),
                    P_ALERT_THRESHOLD_RT => self.rt_alert_threshold(),
                    _ => self.execute_primitive(*id),
                },
                Instruction::Literal(val) => {
                    self.stack.push(*val);
                }
                Instruction::Call(idx) => {
                    let callee = self.dictionary[*idx].body.clone();
                    self.execute_body(&callee);
                }
                Instruction::StringLit(s) => {
                    self.emit_str(s);
                }
                Instruction::Branch(offset) => {
                    ip = (ip as i64 + offset) as usize;
                    continue;
                }
                Instruction::BranchIfZero(offset) => {
                    let flag = self.pop();
                    if flag == 0 {
                        ip = (ip as i64 + offset) as usize;
                        continue;
                    }
                }
            }
            ip += 1;
        }
    }

    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Inner interpreter
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------

    pub(crate) fn execute_primitive(&mut self, id: usize) {
        match id {
            P_DUP => self.prim_dup(),
            P_DROP => self.prim_drop(),
            P_SWAP => self.prim_swap(),
            P_OVER => self.prim_over(),
            P_ROT => self.prim_rot(),
            P_FETCH => self.prim_fetch(),
            P_STORE => self.prim_store(),
            P_ADD => self.prim_add(),
            P_SUB => self.prim_sub(),
            P_MUL => self.prim_mul(),
            P_DIV => self.prim_div(),
            P_MOD => self.prim_mod(),
            P_EQ => self.prim_eq(),
            P_LT => self.prim_lt(),
            P_GT => self.prim_gt(),
            P_AND => self.prim_and(),
            P_OR => self.prim_or(),
            P_NOT => self.prim_not(),
            P_EMIT => self.prim_emit(),
            P_KEY => self.prim_key(),
            P_CR => self.prim_cr(),
            P_DOT => self.prim_dot(),
            P_DOT_S => self.prim_dot_s(),
            P_COLON => self.prim_colon(),
            P_SEMICOLON => self.prim_semicolon(),
            P_CREATE => self.prim_create(),
            P_DOES => self.prim_does(),
            P_VARIABLE => self.prim_variable(),
            P_CONSTANT => self.prim_constant(),
            P_IF => self.prim_if(),
            P_ELSE => self.prim_else(),
            P_THEN => self.prim_then(),
            P_DO => self.prim_do(),
            P_LOOP => self.prim_loop(),
            P_BEGIN => self.prim_begin(),
            P_UNTIL => self.prim_until(),
            P_WHILE => self.prim_while(),
            P_REPEAT => self.prim_repeat(),
            P_DOT_QUOTE => self.prim_dot_quote(),
            P_PAREN => self.prim_paren(),
            P_BACKSLASH => self.prim_backslash(),
            P_WORDS => self.prim_words(),
            P_SEE => self.prim_see(),
            P_QUIT => self.prim_quit(),
            P_BYE => self.prim_bye(),
            P_SEND => self.prim_send(),
            P_RECV => self.prim_recv(),
            P_PEERS => self.prim_peers(),
            P_REPLICATE => self.prim_replicate(),
            P_MUTATE => self.prim_mutate(),
            P_RECURSE => self.prim_recurse(),
            P_MESH_STATUS => self.prim_mesh_status(),
            P_PROPOSE => self.prim_propose(),
            P_LOAD => self.prim_mesh_load(),
            P_CAPACITY => self.prim_mesh_capacity(),
            P_ID => self.prim_id(),
            P_TYPE => self.prim_type(),
            P_GOAL => self.prim_goal(),
            P_GOALS => self.prim_goals(),
            P_TASKS => self.prim_tasks(),
            P_TASK_STATUS => self.prim_task_status(),
            P_CANCEL => self.prim_cancel(),
            P_STEER => self.prim_steer(),
            P_REPORT => self.prim_report(),
            P_CLAIM => self.prim_claim(),
            P_COMPLETE => self.prim_complete(),
            P_GOAL_EXEC => self.prim_goal_exec(),
            P_EVAL => self.prim_eval(),
            P_RESULT => self.prim_result(),
            P_AUTO_CLAIM => self.prim_auto_claim(),
            P_TIMEOUT => self.prim_timeout(),
            P_GOAL_RESULT => self.prim_goal_result(),
            // Host I/O
            P_FILE_READ => self.io_immediate(0),
            P_FILE_WRITE => self.io_immediate(1),
            P_FILE_EXISTS => self.io_immediate(2),
            P_FILE_LIST => self.io_immediate(3),
            P_FILE_DELETE => self.io_immediate(4),
            P_HTTP_GET => self.io_immediate(5),
            P_HTTP_POST => self.io_immediate(6),
            P_SHELL => self.io_immediate(7),
            P_ENV => self.io_immediate(8),
            P_TIMESTAMP => self.prim_timestamp(),
            P_SLEEP => self.prim_sleep(),
            P_SANDBOX_ON => {
                self.sandbox_active = true;
                self.emit_str("sandbox: ON\n");
            }
            P_SANDBOX_OFF => {
                self.sandbox_active = false;
                self.emit_str("sandbox: OFF\n");
            }
            P_IO_LOG => self.prim_io_log(),
            // Mutation
            P_MUTATE_RAND => self.prim_mutate_rand(),
            P_MUTATE_WORD => self.prim_mutate_word(),
            P_UNDO_MUTATE => self.prim_undo_mutate(),
            P_MUTATIONS => self.prim_mutations(),
            // Fitness
            P_FITNESS => {
                let s = self.fitness.score;
                self.stack.push(s);
            }
            P_LEADERBOARD => self.prim_leaderboard(),
            P_RATE => self.prim_rate(),
            P_EVOLVE => self.prim_evolve(),
            P_AUTO_EVOLVE => self.prim_auto_evolve(),
            P_BENCHMARK => self.prim_benchmark(),
            // Trust
            P_TRUST => self.prim_trust(),
            P_TRUST_ALL => {
                self.trusted_peers.clear();
                self.emit_str("trust: ALL (cleared)\n");
            }
            P_TRUST_NONE => {
                self.trusted_peers.clear();
                self.emit_str("trust: NONE\n");
            }
            P_SHELL_ENABLE => {
                self.shell_enabled = !self.shell_enabled;
                self.emit_str(&format!(
                    "shell: {}\n",
                    if self.shell_enabled {
                        "ENABLED"
                    } else {
                        "DISABLED"
                    }
                ));
            }
            // Identity
            P_REIDENTIFY => self.prim_reidentify(),
            // Persistence
            P_SAVE => self.prim_save(),
            P_LOAD_STATE => self.prim_load_state(),
            P_AUTO_SAVE => self.prim_auto_save(),
            P_RESET => self.prim_reset(),
            P_SNAPSHOTS => self.prim_snapshots(),
            P_SNAPSHOT => self.prim_snapshot(),
            P_RESTORE => self.prim_restore(),
            // Loop index: I pushes current DO..LOOP index from return stack
            P_I => {
                let index = self.rstack.last().copied().unwrap_or(0);
                self.stack.push(index);
            }
            // J pushes the outer loop index (2 levels deep on rstack)
            P_J => {
                let len = self.rstack.len();
                let index = if len >= 3 { self.rstack[len - 3] } else { 0 };
                self.stack.push(index);
            }
            // Spawn / Replication
            P_SPAWN => self.prim_spawn(),
            P_SPAWN_N => self.prim_spawn_n(),
            P_PACKAGE => self.prim_package(),
            P_PACKAGE_SIZE => self.prim_package_size(),
            P_CHILDREN => self.prim_children(),
            P_FAMILY => self.prim_family(),
            P_GENERATION => {
                let g = self.spawn_state.generation as Cell;
                self.stack.push(g);
            }
            P_KILL_CHILD => self.prim_kill_child(),
            P_REPLICATE_TO => self.prim_replicate_to(),
            P_ACCEPT_REPL => {
                self.spawn_state.accept_replicate = true;
                self.emit_str("accept-replicate: ON\n");
            }
            P_DENY_REPL => {
                self.spawn_state.accept_replicate = false;
                self.emit_str("accept-replicate: OFF\n");
            }
            P_QUARANTINE => {
                self.spawn_state.quarantine = !self.spawn_state.quarantine;
                self.emit_str(&format!(
                    "quarantine: {}\n",
                    if self.spawn_state.quarantine {
                        "ON"
                    } else {
                        "OFF"
                    }
                ));
            }
            P_MAX_CHILDREN => {
                let n = self.pop() as usize;
                self.spawn_state.max_children = n;
                self.emit_str(&format!("max-children: {}\n", n));
            }
            // Monitoring & Ops
            P_WATCH_URL => self.prim_watch(0),
            P_WATCH_FILE => self.prim_watch(1),
            P_WATCH_PROC => self.prim_watch(2),
            P_WATCHES => {
                let s = self.monitor.format_watches();
                self.emit_str(&s);
            }
            P_UNWATCH => {
                let id = self.pop() as u32;
                self.monitor.remove_watch(id);
                self.emit_str(&format!("watch #{} removed\n", id));
            }
            P_WATCH_LOG => {
                let id = self.pop() as u32;
                let s = self.monitor.format_watch_log(id);
                self.emit_str(&s);
            }
            P_ON_ALERT => self.prim_on_alert(),
            P_ALERTS => {
                let s = self.monitor.format_alerts();
                self.emit_str(&s);
            }
            P_ACK => {
                let id = self.pop() as u32;
                self.monitor.ack_alert(id);
                self.emit_str(&format!("alert #{} acknowledged\n", id));
            }
            P_ALERT_HISTORY => {
                let s = self.monitor.format_alert_history();
                self.emit_str(&s);
            }
            P_DASHBOARD => self.prim_dashboard(),
            P_HEALTH => self.prim_health(),
            P_UPTIME => {
                let id = self.pop() as u32;
                let pct = self.monitor.uptime(id);
                self.emit_str(&format!("watch #{}: {:.1}% uptime\n", id, pct));
            }
            P_EVERY => self.prim_every(),
            P_SCHEDULE => {
                let s = self.monitor.format_schedules();
                self.emit_str(&s);
            }
            P_UNSCHED => {
                let id = self.pop() as u32;
                self.monitor.remove_schedule(id);
                self.emit_str(&format!("schedule #{} removed\n", id));
            }
            P_HEAL => self.prim_heal(),
            P_HEALTH_PORT => {
                let port = self.mesh.as_ref().map(|m| m.repl_port).unwrap_or(0);
                self.stack.push(port as Cell);
            }
            P_ALERT_THRESHOLD => self.prim_alert_threshold(),
            // WebSocket bridge
            P_WS_STATUS => self.prim_ws_status(),
            P_WS_CLIENTS => self.prim_ws_clients(),
            P_WS_PORT => {
                let port = self
                    .ws_state
                    .as_ref()
                    .map(|s| s.lock().unwrap().port as Cell)
                    .unwrap_or(0);
                self.stack.push(port);
            }
            P_WS_BROADCAST => self.prim_ws_broadcast(),
            // Swarm
            P_DISCOVER => self.prim_discover(),
            P_AUTO_DISCOVER => self.prim_auto_discover(),
            P_SHARE_WORD => self.prim_share_word(),
            P_SHARE_ALL => self.prim_share_all(),
            P_AUTO_SHARE => self.prim_auto_share(),
            P_SHARED_WORDS => self.prim_shared_words(),
            P_AUTO_SPAWN_TOGGLE => self.prim_auto_spawn(),
            P_AUTO_CULL_TOGGLE => self.prim_auto_cull(),
            P_MIN_UNITS => self.prim_min_units(),
            P_MAX_UNITS => self.prim_max_units(),
            P_SWARM_STATUS => self.prim_swarm_status(),
            // Replication consent
            P_TRUST_ALL_LEVEL => self.prim_trust_all_level(),
            P_TRUST_MESH => self.prim_trust_mesh(),
            P_TRUST_FAMILY => self.prim_trust_family(),
            P_TRUST_NONE_LEVEL => self.prim_trust_none_level(),
            P_TRUST_LEVEL => self.prim_trust_level(),
            P_REQUESTS => self.prim_requests(),
            P_ACCEPT_REQ => self.prim_accept_req(),
            P_DENY_REQ => self.prim_deny_req(),
            P_DENY_ALL_REQ => self.prim_deny_all_req(),
            P_REPLICATION_LOG => self.prim_replication_log(),
            // Atom primitives
            P_GOAL_COUNT => self.prim_goal_count(),
            P_TASK_COUNT => self.prim_task_count(),
            P_WATCH_COUNT => {
                let n = self.monitor.watches.len() as Cell;
                self.stack.push(n);
            }
            P_ALERT_COUNT => {
                let n = self.monitor.alerts.len() as Cell;
                self.stack.push(n);
            }
            P_CHILD_COUNT => {
                let n = self.spawn_state.children.len() as Cell;
                self.stack.push(n);
            }
            P_MESH_AVG_FITNESS => self.prim_mesh_avg_fitness(),
            P_CHECK_WATCHES => self.prim_check_watches(),
            P_RUN_HANDLERS => self.prim_run_handlers(),
            P_RUN_BENCHMARK => {
                let s = self.run_benchmark();
                self.stack.push(s);
            }
            P_MUTATE_RANDOM => self.prim_mutate_random_atom(),
            P_UNDO_LAST_MUTATION => self.prim_undo_mutate(),
            P_PEER_COUNT => {
                let n = self.mesh.as_ref().map(|m| m.peer_count()).unwrap_or(0) as Cell;
                self.stack.push(n);
            }
            P_SMART_MUTATE => self.prim_smart_mutate(),
            P_MUTATION_REPORT => self.prim_mutation_report(),
            P_MUTATION_STATS => {
                let s = self.mutation_stats.format();
                self.emit_str(&s);
                self.emit_str("\n");
            }
            // Task decomposition
            P_SUBTASK => self.prim_subtask(),
            P_FORK => self.prim_fork(),
            P_RESULTS => self.prim_results(),
            P_REDUCE => self.prim_reduce(),
            P_PROGRESS => self.prim_progress(),
            _ => eprintln!("unknown primitive {}", id),
        }
    }

    // -----------------------------------------------------------------------
    // Stack helpers
    // -----------------------------------------------------------------------
    pub(crate) fn pop(&mut self) -> Cell {
        self.stack.pop().unwrap_or_else(|| {
            self.emit_str("error: stack underflow\n");
            0
        })
    }

    pub(crate) fn rpop(&mut self) -> Cell {
        self.rstack.pop().unwrap_or_else(|| {
            self.emit_str("error: return stack underflow\n");
            0
        })
    }

    // -----------------------------------------------------------------------
    // Output helpers — route output to buffer (sandbox) or stdout
    // -----------------------------------------------------------------------

    pub(crate) fn emit_char(&mut self, ch: char) {
        if let Some(ref mut buf) = self.output_buffer {
            buf.push(ch);
        } else {
            print!("{}", ch);
            let _ = io::stdout().flush();
        }
    }

    pub(crate) fn emit_str(&mut self, s: &str) {
        if let Some(ref mut buf) = self.output_buffer {
            buf.push_str(s);
        } else {
            print!("{}", s);
            let _ = io::stdout().flush();
        }
    }

    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Evaluate Forth code and return captured output.
    pub fn eval(&mut self, input: &str) -> String {
        self.output_buffer = Some(String::new());
        for line in input.lines() {
            self.interpret_line(line);
        }
        self.output_buffer.take().unwrap_or_default()
    }

    /// Return the top of the data stack, or None if empty.
    pub fn stack_top(&self) -> Option<Cell> {
        self.stack.last().copied()
    }
}
