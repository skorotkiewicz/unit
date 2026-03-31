// unit — a software nanobot
// Minimal Forth interpreter that is also a self-replicating networked agent.

// --- Shared types ---
pub mod types;

// --- The Forth VM ---
pub mod vm;

// --- Core nanobot ---
#[allow(dead_code)]
pub mod goals;
#[allow(dead_code)]
pub mod mesh;

// --- Replication & persistence ---
#[allow(dead_code)]
pub mod persist;
#[allow(dead_code)]
pub mod spawn;

// --- Feature layers ---
pub mod features {
    #[allow(dead_code)]
    pub mod fitness;
    #[allow(dead_code)]
    pub mod io_words;
    #[allow(dead_code)]
    pub mod monitor;
    #[allow(dead_code)]
    pub mod mutation;
    #[allow(dead_code)]
    pub mod ws_bridge;
}

#[allow(dead_code)]
mod platform;

#[cfg(target_arch = "wasm32")]
mod wasm_entry;

use std::io::{self, BufRead, Write};

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}
#[cfg(unix)]
unsafe fn libc_kill(pid: i32, sig: i32) -> i32 {
    unsafe { kill(pid, sig) }
}
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use features::{fitness, io_words, monitor, mutation, ws_bridge};
use types::{Cell, Instruction, PAD};
use vm::VM;
use vm::*; // import P_* constants

// ===========================================================================
// Feature primitives — extend the core VM for mesh, goals, I/O, ops, etc.
// ===========================================================================

impl VM {
    // -----------------------------------------------------------------------
    // Atom primitives (raw data for Forth-level orchestration)
    // -----------------------------------------------------------------------

    /// GOAL-COUNT ( -- total pending active completed failed )
    fn prim_goal_count(&mut self) {
        if let Some(ref m) = self.mesh {
            let st = m.state_lock();
            let total = st.goals.goals.len() as Cell;
            let pending = st
                .goals
                .goals
                .values()
                .filter(|g| g.status == goals::GoalStatus::Pending)
                .count() as Cell;
            let active = st
                .goals
                .goals
                .values()
                .filter(|g| g.status == goals::GoalStatus::Active)
                .count() as Cell;
            let completed = st
                .goals
                .goals
                .values()
                .filter(|g| g.status == goals::GoalStatus::Completed)
                .count() as Cell;
            let failed = st
                .goals
                .goals
                .values()
                .filter(|g| g.status == goals::GoalStatus::Failed)
                .count() as Cell;
            drop(st);
            self.stack.push(total);
            self.stack.push(pending);
            self.stack.push(active);
            self.stack.push(completed);
            self.stack.push(failed);
        } else {
            for _ in 0..5 {
                self.stack.push(0);
            }
        }
    }

    /// TASK-COUNT ( -- total waiting running done failed )
    fn prim_task_count(&mut self) {
        if let Some(ref m) = self.mesh {
            let st = m.state_lock();
            let total = st.goals.tasks.len() as Cell;
            let waiting = st
                .goals
                .tasks
                .values()
                .filter(|t| t.status == goals::TaskStatus::Waiting)
                .count() as Cell;
            let running = st
                .goals
                .tasks
                .values()
                .filter(|t| t.status == goals::TaskStatus::Running)
                .count() as Cell;
            let done = st
                .goals
                .tasks
                .values()
                .filter(|t| t.status == goals::TaskStatus::Done)
                .count() as Cell;
            let failed = st
                .goals
                .tasks
                .values()
                .filter(|t| t.status == goals::TaskStatus::Failed)
                .count() as Cell;
            drop(st);
            self.stack.push(total);
            self.stack.push(waiting);
            self.stack.push(running);
            self.stack.push(done);
            self.stack.push(failed);
        } else {
            for _ in 0..5 {
                self.stack.push(0);
            }
        }
    }

    /// MESH-AVG-FITNESS ( -- avg )
    fn prim_mesh_avg_fitness(&mut self) {
        let avg = self
            .mesh
            .as_ref()
            .map(|m| {
                let peers = m.peer_fitness_list();
                if peers.is_empty() {
                    self.fitness.score
                } else {
                    let total: i64 =
                        peers.iter().map(|p| p.score).sum::<i64>() + self.fitness.score;
                    total / (peers.len() as i64 + 1)
                }
            })
            .unwrap_or(0);
        self.stack.push(avg);
    }

    /// CHECK-WATCHES ( -- ) run all due watch checks.
    fn prim_check_watches(&mut self) {
        let due = self.monitor.due_watches();
        for wid in due {
            self.run_watch_check(wid);
        }
    }

    /// RUN-HANDLERS ( -- ) run alert handlers for active alerts.
    fn prim_run_handlers(&mut self) {
        let handlers: Vec<(u32, String)> = self
            .monitor
            .alerts
            .iter()
            .filter(|a| !a.acknowledged)
            .filter_map(|a| {
                self.monitor
                    .watches
                    .get(&a.watch_id)
                    .and_then(|w| w.alert_handler.clone())
                    .map(|h| (a.id, h))
            })
            .collect();
        for (_aid, handler) in &handlers {
            self.interpret_line(handler);
        }
    }

    /// MUTATE-RANDOM ( -- flag ) apply a random mutation, push -1 if success, 0 if fail.
    fn prim_mutate_random_atom(&mut self) {
        let mutable_indices: Vec<usize> = self
            .dictionary
            .iter()
            .enumerate()
            .filter(|(_, e)| mutation::is_mutable(e))
            .map(|(i, _)| i)
            .collect();
        if mutable_indices.is_empty() {
            self.stack.push(0);
            return;
        }
        let idx = mutable_indices[self.rng.next_usize(mutable_indices.len())];
        let dict_len = self.dictionary.len();
        if let Some(mut record) =
            mutation::mutate_entry(&mut self.dictionary[idx], &mut self.rng, dict_len)
        {
            record.word_index = idx;
            self.mutation_history.push(record);
            self.stack.push(-1); // success
        } else {
            self.stack.push(0); // fail
        }
    }

    // -----------------------------------------------------------------------
    // Smart mutation
    // -----------------------------------------------------------------------

    fn snapshot_word(&mut self, idx: usize) -> u64 {
        let body = self.dictionary[idx].body.clone();
        let mut combined = String::new();
        for test_stack in &[vec![], vec![1i64], vec![1, 2, 3]] {
            let saved = std::mem::take(&mut self.stack);
            self.stack = test_stack.clone();
            self.output_buffer = Some(String::new());
            self.timed_out = false;
            self.deadline = Some(Instant::now() + Duration::from_millis(100));
            self.execute_body(&body);
            combined.push_str(&self.output_buffer.take().unwrap_or_default());
            combined.push_str(&format!("{:?}", self.stack));
            self.stack = saved;
            self.deadline = None;
            self.timed_out = false;
        }
        mutation::hash_output(&combined)
    }

    fn prim_smart_mutate(&mut self) {
        let mutable_indices: Vec<usize> = self
            .dictionary
            .iter()
            .enumerate()
            .filter(|(_, e)| mutation::is_mutable(e))
            .map(|(i, _)| i)
            .collect();
        if mutable_indices.is_empty() {
            self.emit_str("no mutable words\n");
            self.stack.push(0);
            return;
        }
        let idx = mutable_indices[self.rng.next_usize(mutable_indices.len())];
        let word_name = self.dictionary[idx].name.clone();
        let before_hash = self.snapshot_word(idx);

        let dict_len = self.dictionary.len();
        let record =
            match mutation::mutate_entry(&mut self.dictionary[idx], &mut self.rng, dict_len) {
                Some(mut r) => {
                    r.word_index = idx;
                    r
                }
                None => {
                    self.stack.push(0);
                    return;
                }
            };

        let after_hash = self.snapshot_word(idx);
        let class = if after_hash == before_hash {
            mutation::MutationClass::Neutral
        } else {
            let score = self.run_benchmark();
            if score >= 0 {
                mutation::MutationClass::Beneficial
            } else {
                mutation::MutationClass::Harmful
            }
        };

        let kept = matches!(
            class,
            mutation::MutationClass::Neutral | mutation::MutationClass::Beneficial
        );
        if kept {
            self.mutation_history.push(record.clone());
        } else {
            mutation::undo_mutation(&mut self.dictionary[idx], &record);
        }

        self.mutation_stats.record(&class);
        self.last_mutation_result = Some(mutation::SmartMutationResult {
            word_name,
            strategy: record.strategy.clone(),
            class,
            before_hash,
            after_hash,
            kept,
            description: record.description,
        });
        self.stack.push(if kept { -1 } else { 0 });
    }

    fn prim_mutation_report(&mut self) {
        if let Some(ref r) = self.last_mutation_result {
            self.emit_str(&format!(
                "last: {} [{}] {} {}\n",
                r.word_name,
                r.strategy.label(),
                r.class.label(),
                if r.kept { "(kept)" } else { "(reverted)" }
            ));
        } else {
            self.emit_str("no mutations yet\n");
        }
    }

    // -----------------------------------------------------------------------
    // Swarm primitives
    // -----------------------------------------------------------------------

    fn prim_discover(&mut self) {
        if let Some(ref m) = self.mesh {
            m.send_discovery_beacon();
            self.emit_str("discovery beacon sent\n");
        }
    }

    fn prim_auto_discover(&mut self) {
        if let Some(ref m) = self.mesh {
            let mut st = m.state_lock();
            st.auto_discover = !st.auto_discover;
            let on = st.auto_discover;
            drop(st);
            self.emit_str(&format!(
                "auto-discover: {}\n",
                if on { "ON" } else { "OFF" }
            ));
        }
    }

    fn prim_share_word(&mut self) {
        let name = self.parse_until('"');
        let upper = name.to_uppercase();
        // Find the word and reconstruct its source (simplified: use SEE-like decompilation).
        if let Some(idx) = self.find_word(&upper) {
            let _entry = &self.dictionary[idx];
            // Build a Forth source representation.
            let source = format!(": {} ;", upper); // simplified — real impl would decompile
            if let Some(ref m) = self.mesh {
                m.share_word(&upper, &source);
                self.emit_str(&format!("shared: {}\n", upper));
            }
        } else {
            self.emit_str(&format!("{}?\n", upper));
        }
    }

    fn prim_share_all(&mut self) {
        if let Some(ref m) = self.mesh {
            // Share all non-kernel words (words with more than one instruction).
            let mut count = 0;
            for entry in &self.dictionary {
                if entry.body.len() > 1 && !entry.hidden {
                    let source = format!(": {} ;", entry.name);
                    m.share_word(&entry.name, &source);
                    count += 1;
                }
            }
            self.emit_str(&format!("shared {} words\n", count));
        }
    }

    fn prim_auto_share(&mut self) {
        if let Some(ref m) = self.mesh {
            let mut st = m.state_lock();
            st.auto_share = !st.auto_share;
            let on = st.auto_share;
            drop(st);
            self.emit_str(&format!("auto-share: {}\n", if on { "ON" } else { "OFF" }));
        }
    }

    fn prim_shared_words(&mut self) {
        if let Some(ref m) = self.mesh {
            let words = m.shared_words_list();
            if words.is_empty() {
                self.emit_str("  (no shared words)\n");
            } else {
                for (name, origin) in &words {
                    self.emit_str(&format!("  {} from {}\n", name, origin));
                }
            }
        }
    }

    fn prim_auto_spawn(&mut self) {
        if let Some(ref m) = self.mesh {
            let mut st = m.state_lock();
            st.auto_spawn = !st.auto_spawn;
            let on = st.auto_spawn;
            drop(st);
            self.emit_str(&format!("auto-spawn: {}\n", if on { "ON" } else { "OFF" }));
        }
    }

    fn prim_auto_cull(&mut self) {
        if let Some(ref m) = self.mesh {
            let mut st = m.state_lock();
            st.auto_cull = !st.auto_cull;
            let on = st.auto_cull;
            drop(st);
            self.emit_str(&format!("auto-cull: {}\n", if on { "ON" } else { "OFF" }));
        }
    }

    fn prim_min_units(&mut self) {
        let n = self.pop() as usize;
        if let Some(ref m) = self.mesh {
            let mut st = m.state_lock();
            st.min_units = n.max(1);
            drop(st);
            self.emit_str(&format!("min-units: {}\n", n));
        }
    }

    fn prim_max_units(&mut self) {
        let n = self.pop() as usize;
        if let Some(ref m) = self.mesh {
            let mut st = m.state_lock();
            st.max_units = n.max(1);
            drop(st);
            self.emit_str(&format!("max-units: {}\n", n));
        }
    }

    fn prim_swarm_status(&mut self) {
        if let Some(ref m) = self.mesh {
            let s = m.format_swarm_status();
            self.emit_str(&s);
        } else {
            self.emit_str("swarm: offline\n");
        }
    }

    /// Compile shared words received from peers.
    fn process_shared_words(&mut self) {
        let words = self
            .mesh
            .as_ref()
            .map(|m| m.recv_shared_words())
            .unwrap_or_default();
        for word in words {
            // Compile the shared word source.
            self.interpret_line(&word.body_source);
        }
    }

    /// Swarm tick — process word shares and check autonomous behaviors.
    fn tick_swarm(&mut self) {
        self.process_shared_words();
    }

    // -----------------------------------------------------------------------
    // Replication consent primitives
    // -----------------------------------------------------------------------

    fn prim_trust_all_level(&mut self) {
        if let Some(ref m) = self.mesh {
            m.set_trust_level(mesh::TrustLevel::All);
            self.emit_str("trust: ALL (auto-accept everything)\n");
        }
    }

    fn prim_trust_mesh(&mut self) {
        if let Some(ref m) = self.mesh {
            m.set_trust_level(mesh::TrustLevel::Mesh);
            self.emit_str("trust: MESH (auto-accept known peers)\n");
        }
    }

    fn prim_trust_family(&mut self) {
        if let Some(ref m) = self.mesh {
            m.set_trust_level(mesh::TrustLevel::Family);
            self.emit_str("trust: FAMILY (auto-accept parent/children)\n");
        }
    }

    fn prim_trust_none_level(&mut self) {
        if let Some(ref m) = self.mesh {
            m.set_trust_level(mesh::TrustLevel::None);
            self.emit_str("trust: NONE (manual approval required)\n");
        }
    }

    fn prim_trust_level(&mut self) {
        if let Some(ref m) = self.mesh {
            let level = m.trust_level();
            self.stack.push(level.as_val());
            self.emit_str(&format!("trust: {}\n", level.label()));
        } else {
            self.stack.push(0);
        }
    }

    fn prim_requests(&mut self) {
        if let Some(ref m) = self.mesh {
            let s = m.format_requests();
            self.emit_str(&s);
        }
    }

    fn prim_accept_req(&mut self) {
        if let Some(ref m) = self.mesh {
            if let Some((sender, rid)) = m.accept_oldest() {
                self.emit_str(&format!(
                    "accepted request #{} from {}\n",
                    rid,
                    mesh::id_to_hex(&sender)
                ));
            } else {
                self.emit_str("no pending requests\n");
            }
        }
    }

    fn prim_deny_req(&mut self) {
        if let Some(ref m) = self.mesh {
            if let Some(rid) = m.deny_oldest() {
                self.emit_str(&format!("denied request #{}\n", rid));
            } else {
                self.emit_str("no pending requests\n");
            }
        }
    }

    fn prim_deny_all_req(&mut self) {
        if let Some(ref m) = self.mesh {
            let n = m.deny_all_requests();
            self.emit_str(&format!("denied {} request(s)\n", n));
        }
    }

    fn prim_replication_log(&mut self) {
        if let Some(ref m) = self.mesh {
            let s = m.format_replication_log();
            self.emit_str(&s);
        }
    }

    // -----------------------------------------------------------------------
    // Mesh primitives
    // -----------------------------------------------------------------------

    /// SEND ( addr n peer -- ) send n bytes from memory to all peers.
    /// The peer argument is reserved for future use (ignored, broadcast).
    fn prim_send(&mut self) {
        let _peer = self.pop(); // reserved
        let n = self.pop() as usize;
        let addr = self.pop() as usize;

        // Read n cells from memory, convert each to a byte.
        let mut data = Vec::with_capacity(n);
        for i in 0..n {
            let a = addr + i;
            if a < self.memory.len() {
                data.push(self.memory[a] as u8);
            }
        }

        if let Some(ref m) = self.mesh {
            m.send_data(&data);
        } else {
            eprintln!("SEND: mesh offline");
        }
    }

    /// RECV ( -- addr n peer ) receive next message.
    /// Copies data to PAD buffer. peer is the sender (0 = none).
    fn prim_recv(&mut self) {
        if let Some(ref m) = self.mesh {
            if let Some(msg) = m.recv_data() {
                // Copy data to PAD area in memory.
                let len = msg.data.len().min(self.memory.len() - PAD);
                for (i, &byte) in msg.data.iter().take(len).enumerate() {
                    self.memory[PAD + i] = byte as Cell;
                }
                self.stack.push(PAD as Cell);
                self.stack.push(len as Cell);
                // Push a nonzero "peer" value to indicate a message was received.
                self.stack.push(-1);
                return;
            }
        }
        // No message or mesh offline.
        self.stack.push(0);
        self.stack.push(0);
        self.stack.push(0);
    }

    /// PEERS ( -- n ) number of known peers.
    fn prim_peers(&mut self) {
        let count = self.mesh.as_ref().map_or(0, |m| m.peer_count());
        self.stack.push(count as Cell);
    }

    /// REPLICATE ( -- ) serialize this unit's state and broadcast to peers.
    fn prim_replicate(&mut self) {
        if let Some(ref m) = self.mesh {
            // Update load metric before serializing.
            let user_words = self.dictionary.len();
            m.set_load(user_words as u32);

            let goals = m.clone_goals();
            let state_bytes =
                mesh::serialize_state(&self.dictionary, &self.memory, self.here, Some(&goals));
            println!(
                "REPLICATE: serialized {} bytes ({} dictionary entries, {} memory cells)",
                state_bytes.len(),
                self.dictionary.len(),
                self.here
            );
            m.send_data(&state_bytes);
        } else {
            eprintln!("REPLICATE: mesh offline");
        }
    }

    /// MUTATE ( xt -- ) replace a word's definition at runtime.
    /// Stub: prints info about what would happen.
    fn prim_mutate(&mut self) {
        let xt = self.pop() as usize;
        if xt < self.dictionary.len() {
            let name = &self.dictionary[xt].name;
            eprintln!(
                "MUTATE: would replace definition of {} (xt={}). Not yet implemented.",
                name, xt
            );
        } else {
            eprintln!("MUTATE: invalid xt {}", xt);
        }
    }

    /// MESH-STATUS ( -- ) print mesh state.
    fn prim_mesh_status(&mut self) {
        if let Some(ref m) = self.mesh {
            let s = m.format_status();
            self.emit_str(&s);
        } else {
            self.emit_str("mesh: offline\n");
        }
    }

    /// PROPOSE ( -- ) trigger a replication proposal via consensus.
    fn prim_propose(&mut self) {
        if let Some(ref m) = self.mesh {
            // Update load metric.
            let user_words = self.dictionary.len();
            m.set_load(user_words as u32);

            // Serialize state for the proposal.
            let goals = m.clone_goals();
            let state_bytes =
                mesh::serialize_state(&self.dictionary, &self.memory, self.here, Some(&goals));
            let reason = format!("load={} dict_size={}", user_words, self.dictionary.len());

            match m.propose_replicate(&reason, state_bytes) {
                Ok(()) => println!("PROPOSE: proposal submitted to mesh"),
                Err(e) => eprintln!("PROPOSE: {}", e),
            }
        } else {
            eprintln!("PROPOSE: mesh offline");
        }
    }

    /// LOAD ( -- n ) push current load metric.
    fn prim_mesh_load(&mut self) {
        let load = self.mesh.as_ref().map_or(0, |m| m.load());
        self.stack.push(load as Cell);
    }

    /// CAPACITY ( -- n ) push capacity threshold.
    fn prim_mesh_capacity(&mut self) {
        let cap = self.mesh.as_ref().map_or(0, |m| m.capacity());
        self.stack.push(cap as Cell);
    }

    /// ID ( -- addr n ) push this unit's ID string to PAD and return addr+len.
    fn prim_id(&mut self) {
        let id_str = self
            .mesh
            .as_ref()
            .map_or_else(|| "offline".to_string(), |m| m.id_hex().to_string());

        // Write to PAD area.
        let len = id_str.len().min(self.memory.len() - PAD);
        for (i, byte) in id_str.bytes().take(len).enumerate() {
            self.memory[PAD + i] = byte as Cell;
        }
        self.stack.push(PAD as Cell);
        self.stack.push(len as Cell);
    }

    /// TYPE ( addr n -- ) print n characters from memory starting at addr.
    fn prim_type(&mut self) {
        let n = self.pop() as usize;
        let addr = self.pop() as usize;
        for i in 0..n {
            let a = addr + i;
            if a < self.memory.len() {
                self.emit_char(self.memory[a] as u8 as char);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Sandbox execution engine
    // -----------------------------------------------------------------------

    /// Parse balanced braces from the input buffer. Returns the content
    /// between the opening { (already consumed) and the closing }.
    fn parse_balanced_braces(&mut self) -> String {
        let bytes = self.input_buffer.as_bytes();
        if self.input_pos < bytes.len() && bytes[self.input_pos] == b' ' {
            self.input_pos += 1;
        }
        let start = self.input_pos;
        let mut depth = 1i32;
        while self.input_pos < bytes.len() && depth > 0 {
            match bytes[self.input_pos] as char {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let result = self.input_buffer[start..self.input_pos].to_string();
                        self.input_pos += 1;
                        return result;
                    }
                }
                _ => {}
            }
            self.input_pos += 1;
        }
        self.input_buffer[start..self.input_pos].to_string()
    }

    /// Execute Forth code in a sandbox. Saves/restores VM state. Returns
    /// a TaskResult with the captured stack, output, and success status.
    fn execute_sandbox(&mut self, code: &str) -> goals::TaskResult {
        // Save state.
        let saved_stack = std::mem::take(&mut self.stack);
        let saved_rstack = std::mem::take(&mut self.rstack);
        let saved_silent = self.silent;
        let saved_compiling = self.compiling;
        let saved_current_def = self.current_def.take();
        let saved_output_buffer = self.output_buffer.take();
        let saved_deadline = self.deadline.take();
        let saved_timed_out = self.timed_out;
        let saved_sandbox = self.sandbox_active;

        // Set up sandbox.
        self.stack = Vec::with_capacity(256);
        self.rstack = Vec::with_capacity(256);
        self.output_buffer = Some(String::new());
        self.silent = true;
        self.sandbox_active = true; // remote code always sandboxed
        self.compiling = false;
        self.timed_out = false;
        self.deadline = Some(Instant::now() + Duration::from_secs(self.execution_timeout));

        // Execute.
        for line in code.lines() {
            self.interpret_line(line);
            if self.timed_out || !self.running {
                break;
            }
        }

        // Capture results.
        let stack_snapshot = self.stack.clone();
        let output = self.output_buffer.take().unwrap_or_default();
        let success = !self.timed_out;
        let error = if self.timed_out {
            Some(format!("execution timeout ({}s)", self.execution_timeout))
        } else {
            None
        };

        // Restore state.
        self.stack = saved_stack;
        self.rstack = saved_rstack;
        self.silent = saved_silent;
        self.compiling = saved_compiling;
        self.current_def = saved_current_def;
        self.output_buffer = saved_output_buffer;
        self.deadline = saved_deadline;
        self.timed_out = saved_timed_out;
        self.sandbox_active = saved_sandbox;
        self.running = true; // task execution must not kill the unit

        goals::TaskResult {
            stack_snapshot,
            output,
            success,
            error,
        }
    }

    // -----------------------------------------------------------------------
    // Goal primitives
    // -----------------------------------------------------------------------

    /// GOAL" <description>" ( priority -- goal-id ) submit a description-only goal.
    fn prim_goal(&mut self) {
        let desc = self.parse_until('"');
        let priority = self.pop();
        if let Some(ref m) = self.mesh {
            let goal_id = m.create_goal(&desc, priority, None);
            m.set_load(self.dictionary.len() as u32);
            self.stack.push(goal_id as Cell);
            if !self.silent {
                println!("goal #{} created", goal_id);
            }
        } else {
            eprintln!("GOAL: mesh offline");
            self.stack.push(0);
        }
    }

    /// GOALS ( -- ) list all known goals.
    fn prim_goals(&mut self) {
        if let Some(ref m) = self.mesh {
            let s = m.format_goals();
            self.emit_str(&s);
        } else {
            self.emit_str("  (mesh offline)\n");
        }
    }

    /// TASKS ( -- ) list this unit's current task queue.
    fn prim_tasks(&mut self) {
        if let Some(ref m) = self.mesh {
            let s = m.format_tasks();
            self.emit_str(&s);
        } else {
            self.emit_str("  (mesh offline)\n");
        }
    }

    /// TASK-STATUS ( goal-id -- ) show task breakdown for a specific goal.
    fn prim_task_status(&mut self) {
        let goal_id = self.pop() as u64;
        if let Some(ref m) = self.mesh {
            print!("{}", m.format_goal_tasks(goal_id));
            let _ = io::stdout().flush();
        } else {
            eprintln!("TASK-STATUS: mesh offline");
        }
    }

    /// CANCEL ( goal-id -- ) cancel a goal and all its tasks.
    fn prim_cancel(&mut self) {
        let goal_id = self.pop() as u64;
        if let Some(ref m) = self.mesh {
            if m.cancel_goal(goal_id) {
                println!("goal #{} cancelled", goal_id);
            } else {
                eprintln!("goal #{} not found", goal_id);
            }
        } else {
            eprintln!("CANCEL: mesh offline");
        }
    }

    /// STEER ( goal-id priority -- ) change priority of a goal.
    fn prim_steer(&mut self) {
        let priority = self.pop();
        let goal_id = self.pop() as u64;
        if let Some(ref m) = self.mesh {
            if m.steer_goal(goal_id, priority) {
                println!("goal #{} priority -> {}", goal_id, priority);
            } else {
                eprintln!("goal #{} not found", goal_id);
            }
        } else {
            eprintln!("STEER: mesh offline");
        }
    }

    /// REPORT ( -- ) mesh-wide progress summary.
    fn prim_report(&mut self) {
        if let Some(ref m) = self.mesh {
            print!("{}", m.format_report());
            let _ = io::stdout().flush();
        } else {
            println!("  (mesh offline)");
        }
    }

    /// CLAIM ( -- task-id ) claim the next available task, or 0 if none.
    /// CLAIM ( -- task-id ) claim and execute the next available task.
    fn prim_claim(&mut self) {
        // Extract claimed task info (releases mesh borrow).
        let claimed = self.mesh.as_ref().and_then(|m| m.claim_task());

        if let Some((task_id, goal_id, desc)) = claimed {
            println!("claimed task #{} (goal #{}): {}", task_id, goal_id, desc);
            // Check if the parent goal has executable code.
            let code = self.mesh.as_ref().and_then(|m| m.goal_code(goal_id));
            if let Some(code) = code {
                let result = self.execute_sandbox(&code);
                if !result.output.is_empty() {
                    println!("  output: {}", result.output.trim_end());
                }
                if !result.stack_snapshot.is_empty() {
                    print!("  stack: ");
                    for v in &result.stack_snapshot {
                        print!("{} ", v);
                    }
                    println!();
                }
                if !result.success {
                    println!("  FAILED: {}", result.error.as_deref().unwrap_or("unknown"));
                }
                if let Some(ref m) = self.mesh {
                    m.complete_task_with_result(task_id, result);
                }
            }
            self.stack.push(task_id as Cell);
        } else {
            println!("no tasks available");
            self.stack.push(0);
        }
    }

    /// COMPLETE ( task-id -- ) mark a task as done.
    fn prim_complete(&mut self) {
        let task_id = self.pop() as u64;
        if let Some(ref m) = self.mesh {
            m.complete_task_with_result(
                task_id,
                goals::TaskResult {
                    stack_snapshot: vec![],
                    output: String::new(),
                    success: true,
                    error: None,
                },
            );
            println!("task #{} completed", task_id);
        } else {
            eprintln!("COMPLETE: mesh offline");
        }
    }

    /// GOAL{ <forth code> } ( priority -- goal-id ) submit an executable goal.
    /// Immediate: parses the code at compile time. In compile mode, stores
    /// the code in a side table and compiles Literal(index) + Primitive(RT).
    fn prim_goal_exec(&mut self) {
        let code = self.parse_balanced_braces();
        if self.compiling {
            let idx = self.code_strings.len();
            self.code_strings.push(code);
            if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::Literal(idx as Cell));
                def.body.push(Instruction::Primitive(P_GOAL_EXEC_RT));
            }
        } else {
            self.create_exec_goal(&code);
        }
    }

    /// Runtime primitive for compiled GOAL{. Pops code-string index from
    /// stack, looks up the code, then creates the goal.
    fn rt_goal_exec(&mut self) {
        let idx = self.pop() as usize;
        if idx < self.code_strings.len() {
            let code = self.code_strings[idx].clone();
            self.create_exec_goal(&code);
        } else {
            eprintln!("GOAL{{: invalid code index");
            self.stack.push(0);
        }
    }

    fn create_exec_goal(&mut self, code: &str) {
        let priority = self.pop();

        // Check for SPLIT directive in the code.
        if let Some(split_pos) = code.find(" SPLIT ") {
            let before = &code[..split_pos];
            let after = &code[split_pos + 7..]; // skip " SPLIT "
                                                // Evaluate the "before" part to get total and N from the stack.
            let saved = self.stack.clone();
            self.interpret_line(before);
            let n = self.pop();
            let total = self.pop();
            self.stack = saved;

            if n > 0 && total > 0 {
                if let Some(ref m) = self.mesh {
                    let mut st = m.state_lock();
                    let goal_id =
                        st.goals
                            .create_split_goal(total, n, after, priority, m.id_bytes());
                    drop(st);
                    m.set_load(self.dictionary.len() as u32);
                    self.stack.push(goal_id as Cell);
                    if !self.silent {
                        println!(
                            "goal #{} created [split {}×{}]: {}",
                            goal_id,
                            n,
                            total / n,
                            after.chars().take(40).collect::<String>()
                        );
                    }
                    return;
                }
            }
        }

        // Normal (non-SPLIT) goal creation.
        if let Some(ref m) = self.mesh {
            let goal_id = m.create_goal(code, priority, Some(code.to_string()));
            m.set_load(self.dictionary.len() as u32);
            self.stack.push(goal_id as Cell);
            if !self.silent {
                println!(
                    "goal #{} created [exec]: {}",
                    goal_id,
                    code.chars().take(60).collect::<String>()
                );
            }
        } else {
            eprintln!("GOAL: mesh offline");
            self.stack.push(0);
        }
    }

    /// EVAL" <forth code>" ( -- ) evaluate a string of Forth immediately.
    fn prim_eval(&mut self) {
        let code = self.parse_until('"');
        self.interpret_line(&code);
    }

    /// RESULT ( task-id -- ) display the result of a completed task.
    fn prim_result(&mut self) {
        let task_id = self.pop() as u64;
        if let Some(ref m) = self.mesh {
            let s = m.format_task_result(task_id);
            self.emit_str(&s);
        } else {
            eprintln!("RESULT: mesh offline");
        }
    }

    /// AUTO-CLAIM ( -- ) toggle automatic task claiming and execution.
    fn prim_auto_claim(&mut self) {
        self.auto_claim = !self.auto_claim;
        if !self.silent {
            println!("auto-claim: {}", if self.auto_claim { "ON" } else { "OFF" });
        }
    }

    /// TIMEOUT ( seconds -- ) set execution timeout for sandboxed tasks.
    fn prim_timeout(&mut self) {
        let secs = self.pop();
        if secs > 0 {
            self.execution_timeout = secs as u64;
            if !self.silent {
                println!("execution timeout: {}s", self.execution_timeout);
            }
        } else {
            eprintln!("TIMEOUT: must be > 0");
        }
    }

    /// GOAL-RESULT ( goal-id -- ) show combined results from all tasks of a goal.
    fn prim_goal_result(&mut self) {
        let goal_id = self.pop() as u64;
        if let Some(ref m) = self.mesh {
            let s = m.format_goal_result(goal_id);
            self.emit_str(&s);
        } else {
            eprintln!("GOAL-RESULT: mesh offline");
        }
    }

    /// Check for and execute auto-claimed tasks.
    fn check_auto_claim(&mut self) {
        if !self.auto_claim {
            return;
        }
        // Extract the claimed task info while borrowing mesh immutably.
        let claimed = self.mesh.as_ref().and_then(|m| m.claim_executable_task());

        if let Some((task_id, goal_id, desc, code)) = claimed {
            println!(
                "[auto] claimed task #{} (goal #{}): {}",
                task_id,
                goal_id,
                desc.chars().take(50).collect::<String>()
            );
            // Execute in sandbox with timing.
            let start = Instant::now();
            let result = self.execute_sandbox(&code);
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let success = result.success;

            // Record fitness.
            if success {
                self.fitness.record_success(elapsed_ms);
            } else {
                self.fitness.record_failure();
            }
            if !result.output.is_empty() {
                println!("[auto] output: {}", result.output.trim_end());
            }
            if !result.stack_snapshot.is_empty() {
                print!("[auto] stack: ");
                for v in &result.stack_snapshot {
                    print!("{} ", v);
                }
                println!();
            }
            if !success {
                println!(
                    "[auto] FAILED: {}",
                    result.error.as_deref().unwrap_or("unknown")
                );
            }
            // Now borrow mesh again to broadcast result.
            if let Some(ref m) = self.mesh {
                m.complete_task_with_result(task_id, result);
                m.set_fitness(self.fitness.score);
            }
            self.check_auto_save();
            println!("[auto] task #{} done", task_id);
        }
    }

    /// Check if auto-replication should be triggered by goal load.
    fn check_auto_replicate(&mut self) {
        let should = self
            .mesh
            .as_ref()
            .is_some_and(|m| m.should_auto_replicate());
        if should {
            if let Some(ref m) = self.mesh {
                m.clear_auto_replicate();
                m.set_load(self.dictionary.len() as u32);
                let goals = m.clone_goals();
                let state_bytes =
                    mesh::serialize_state(&self.dictionary, &self.memory, self.here, Some(&goals));
                let reason = format!("auto: goal_load dict={}", self.dictionary.len());
                match m.propose_replicate(&reason, state_bytes) {
                    Ok(()) => println!("auto-replication proposed"),
                    Err(e) => {
                        if !self.silent {
                            eprintln!("auto-replicate: {}", e);
                        }
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Host I/O primitives
    // -----------------------------------------------------------------------

    fn log_io(&mut self, msg: &str) {
        self.io_log.push_back(msg.to_string());
        if self.io_log.len() > 50 {
            self.io_log.pop_front();
        }
    }

    fn check_sandbox_write(&self, op: &str) -> bool {
        if self.sandbox_active {
            eprintln!("{}: blocked by sandbox", op);
            false
        } else {
            true
        }
    }

    fn check_shell_allowed(&self) -> bool {
        if self.sandbox_active {
            eprintln!("SHELL: blocked by sandbox");
            return false;
        }
        if !self.shell_enabled {
            eprintln!("SHELL: disabled (use SHELL-ENABLE from REPL)");
            return false;
        }
        true
    }

    /// Common handler for all immediate I/O words. Parses the string,
    /// and in compile mode stores it for runtime dispatch.
    fn io_immediate(&mut self, op: Cell) {
        let s = self.parse_until('"');
        if self.compiling {
            let idx = self.code_strings.len();
            self.code_strings.push(s);
            if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::Literal(idx as Cell));
                def.body.push(Instruction::Literal(op));
                def.body.push(Instruction::Primitive(P_IO_RT));
            }
        } else {
            self.execute_io(op, &s);
        }
    }

    /// Runtime dispatch for compiled I/O words.
    fn rt_io(&mut self) {
        let op = self.pop();
        let idx = self.pop() as usize;
        if idx < self.code_strings.len() {
            let s = self.code_strings[idx].clone();
            self.execute_io(op, &s);
        }
    }

    fn execute_io(&mut self, op: Cell, s: &str) {
        match op {
            0 => self.do_file_read(s),
            1 => self.do_file_write(s),
            2 => self.do_file_exists(s),
            3 => self.do_file_list(s),
            4 => self.do_file_delete(s),
            5 => self.do_http_get(s),
            6 => self.do_http_post(s),
            7 => self.do_shell(s),
            8 => self.do_env(s),
            _ => {}
        }
    }

    fn do_file_read(&mut self, path: &str) {
        self.log_io(&format!("FILE-READ {}", path));
        match io_words::file_read(path) {
            Ok(data) => {
                let len = data.len().min(self.memory.len() - PAD);
                for (i, &byte) in data.iter().take(len).enumerate() {
                    self.memory[PAD + i] = byte as Cell;
                }
                self.stack.push(PAD as Cell);
                self.stack.push(len as Cell);
            }
            Err(e) => {
                if !self.silent {
                    eprintln!("FILE-READ: {}", e);
                }
                self.stack.push(0);
                self.stack.push(0);
            }
        }
    }

    fn do_file_write(&mut self, path: &str) {
        if !self.check_sandbox_write("FILE-WRITE") {
            return;
        }
        let n = self.pop() as usize;
        let addr = self.pop() as usize;
        let mut data = Vec::with_capacity(n);
        for i in 0..n {
            if addr + i < self.memory.len() {
                data.push(self.memory[addr + i] as u8);
            }
        }
        self.log_io(&format!("FILE-WRITE {} ({} bytes)", path, n));
        if let Err(e) = io_words::file_write(path, &data) {
            if !self.silent {
                eprintln!("FILE-WRITE: {}", e);
            }
        }
    }

    fn do_file_exists(&mut self, path: &str) {
        self.log_io(&format!("FILE-EXISTS {}", path));
        let flag = if io_words::file_exists(path) { -1 } else { 0 };
        self.stack.push(flag);
    }

    fn do_file_list(&mut self, path: &str) {
        self.log_io(&format!("FILE-LIST {}", path));
        match io_words::file_list(path) {
            Ok(names) => {
                for name in &names {
                    self.emit_str(&format!("  {}\n", name));
                }
            }
            Err(e) => {
                if !self.silent {
                    eprintln!("FILE-LIST: {}", e);
                }
            }
        }
    }

    fn do_file_delete(&mut self, path: &str) {
        if !self.check_sandbox_write("FILE-DELETE") {
            self.stack.push(0);
            return;
        }
        self.log_io(&format!("FILE-DELETE {}", path));
        let flag = if io_words::file_delete(path).is_ok() {
            -1
        } else {
            0
        };
        self.stack.push(flag);
    }

    fn do_http_get(&mut self, url: &str) {
        self.log_io(&format!("HTTP-GET {}", url));
        match io_words::http_get(url) {
            Ok((body, status)) => {
                let len = body.len().min(self.memory.len() - PAD);
                for (i, &byte) in body.iter().take(len).enumerate() {
                    self.memory[PAD + i] = byte as Cell;
                }
                self.stack.push(PAD as Cell);
                self.stack.push(len as Cell);
                self.stack.push(status as Cell);
            }
            Err(e) => {
                if !self.silent {
                    eprintln!("HTTP-GET: {}", e);
                }
                self.stack.push(0);
                self.stack.push(0);
                self.stack.push(0);
            }
        }
    }

    fn do_http_post(&mut self, url: &str) {
        if !self.check_sandbox_write("HTTP-POST") {
            self.stack.push(0);
            self.stack.push(0);
            self.stack.push(0);
            return;
        }
        let n = self.pop() as usize;
        let addr = self.pop() as usize;
        let mut body = Vec::with_capacity(n);
        for i in 0..n {
            if addr + i < self.memory.len() {
                body.push(self.memory[addr + i] as u8);
            }
        }
        self.log_io(&format!("HTTP-POST {} ({} bytes)", url, n));
        match io_words::http_post(url, &body) {
            Ok((resp, status)) => {
                let len = resp.len().min(self.memory.len() - PAD);
                for (i, &byte) in resp.iter().take(len).enumerate() {
                    self.memory[PAD + i] = byte as Cell;
                }
                self.stack.push(PAD as Cell);
                self.stack.push(len as Cell);
                self.stack.push(status as Cell);
            }
            Err(e) => {
                if !self.silent {
                    eprintln!("HTTP-POST: {}", e);
                }
                self.stack.push(0);
                self.stack.push(0);
                self.stack.push(0);
            }
        }
    }

    fn do_shell(&mut self, cmd: &str) {
        if !self.check_shell_allowed() {
            self.stack.push(0);
            self.stack.push(0);
            self.stack.push(-1);
            return;
        }
        self.log_io(&format!("SHELL {}", cmd));
        match io_words::shell_exec(cmd) {
            Ok((stdout, exit_code)) => {
                let len = stdout.len().min(self.memory.len() - PAD);
                for (i, &byte) in stdout.iter().take(len).enumerate() {
                    self.memory[PAD + i] = byte as Cell;
                }
                self.stack.push(PAD as Cell);
                self.stack.push(len as Cell);
                self.stack.push(exit_code as Cell);
            }
            Err(e) => {
                if !self.silent {
                    eprintln!("SHELL: {}", e);
                }
                self.stack.push(0);
                self.stack.push(0);
                self.stack.push(-1);
            }
        }
    }

    fn do_env(&mut self, name: &str) {
        self.log_io(&format!("ENV {}", name));
        if let Some(val) = io_words::env_var(name) {
            let len = val.len().min(self.memory.len() - PAD);
            for (i, byte) in val.bytes().take(len).enumerate() {
                self.memory[PAD + i] = byte as Cell;
            }
            self.stack.push(PAD as Cell);
            self.stack.push(len as Cell);
        } else {
            self.stack.push(0);
            self.stack.push(0);
        }
    }

    fn prim_timestamp(&mut self) {
        self.stack.push(io_words::timestamp());
    }

    fn prim_sleep(&mut self) {
        let ms = self.pop();
        if ms > 0 {
            std::thread::sleep(Duration::from_millis(ms as u64));
        }
    }

    fn prim_io_log(&mut self) {
        if self.io_log.is_empty() {
            self.emit_str("  (no I/O operations logged)\n");
        } else {
            self.emit_str("--- I/O log ---\n");
            let entries: Vec<String> = self.io_log.iter().cloned().collect();
            for entry in &entries {
                self.emit_str(&format!("  {}\n", entry));
            }
            self.emit_str("---\n");
        }
    }

    // -----------------------------------------------------------------------
    // Mutation primitives
    // -----------------------------------------------------------------------

    fn prim_mutate_rand(&mut self) {
        // Pick a random mutable word.
        let mutable_indices: Vec<usize> = self
            .dictionary
            .iter()
            .enumerate()
            .filter(|(_, e)| mutation::is_mutable(e))
            .map(|(i, _)| i)
            .collect();
        if mutable_indices.is_empty() {
            self.emit_str("no mutable words\n");
            return;
        }
        let idx = mutable_indices[self.rng.next_usize(mutable_indices.len())];
        let dict_len = self.dictionary.len();
        if let Some(mut record) =
            mutation::mutate_entry(&mut self.dictionary[idx], &mut self.rng, dict_len)
        {
            record.word_index = idx;
            self.emit_str(&format!("mutated: {}\n", record.format()));
            self.mutation_history.push(record);
        } else {
            self.emit_str("mutation failed (no applicable strategy)\n");
        }
    }

    fn prim_mutate_word(&mut self) {
        let name = self.parse_until('"');
        if self.compiling {
            let idx = self.code_strings.len();
            self.code_strings.push(name);
            if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::Literal(idx as Cell));
                def.body.push(Instruction::Primitive(P_MUTATE_WORD_RT));
            }
        } else {
            self.do_mutate_word(&name);
        }
    }

    fn rt_mutate_word(&mut self) {
        let idx = self.pop() as usize;
        if idx < self.code_strings.len() {
            let name = self.code_strings[idx].clone();
            self.do_mutate_word(&name);
        }
    }

    fn do_mutate_word(&mut self, name: &str) {
        let upper = name.to_uppercase();
        if let Some(idx) = self.find_word(&upper) {
            if !mutation::is_mutable(&self.dictionary[idx]) {
                self.emit_str(&format!("{}: not mutable (kernel word)\n", upper));
                return;
            }
            let dict_len = self.dictionary.len();
            if let Some(mut record) =
                mutation::mutate_entry(&mut self.dictionary[idx], &mut self.rng, dict_len)
            {
                record.word_index = idx;
                self.emit_str(&format!("mutated: {}\n", record.format()));
                self.mutation_history.push(record);
            } else {
                self.emit_str("mutation failed\n");
            }
        } else {
            self.emit_str(&format!("{}?\n", upper));
        }
    }

    fn prim_undo_mutate(&mut self) {
        if let Some(record) = self.mutation_history.pop() {
            if record.word_index < self.dictionary.len() {
                mutation::undo_mutation(&mut self.dictionary[record.word_index], &record);
                self.emit_str(&format!(
                    "undone: {} [{}]\n",
                    record.word_name,
                    record.strategy.label()
                ));
            }
        } else {
            self.emit_str("nothing to undo\n");
        }
    }

    fn prim_mutations(&mut self) {
        if self.mutation_history.is_empty() {
            self.emit_str("  (no mutations)\n");
        } else {
            let lines: Vec<String> = self.mutation_history.iter().map(|r| r.format()).collect();
            for line in &lines {
                self.emit_str(&format!("{}\n", line));
            }
        }
    }

    // -----------------------------------------------------------------------
    // Fitness / Evolution primitives
    // -----------------------------------------------------------------------

    fn prim_leaderboard(&mut self) {
        if let Some(ref m) = self.mesh {
            let peer_fitness = m.peer_fitness_list();
            let s = fitness::format_leaderboard(&m.id_bytes(), self.fitness.score, &peer_fitness);
            self.emit_str(&s);
        } else {
            self.emit_str(&format!("  (offline) score={}\n", self.fitness.score));
        }
    }

    fn prim_rate(&mut self) {
        let score = self.pop();
        let _task_id = self.pop() as u64;
        // For now, rating adjusts local fitness (the rated peer would
        // receive the rating via gossip in a fuller implementation).
        self.fitness.record_rating(score);
        self.emit_str(&format!("rated: fitness adjusted by {}\n", score));
    }

    fn prim_evolve(&mut self) {
        self.do_evolve();
    }

    fn prim_auto_evolve(&mut self) {
        self.fitness.auto_evolve = !self.fitness.auto_evolve;
        self.emit_str(&format!(
            "auto-evolve: {}\n",
            if self.fitness.auto_evolve {
                "ON"
            } else {
                "OFF"
            }
        ));
    }

    fn prim_benchmark(&mut self) {
        let code = self.parse_until('"');
        if self.compiling {
            let idx = self.code_strings.len();
            self.code_strings.push(code);
            if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::Literal(idx as Cell));
                def.body.push(Instruction::Primitive(P_BENCHMARK_RT));
            }
        } else {
            self.fitness.benchmark_code = Some(code.clone());
            self.emit_str(&format!(
                "benchmark set: {}\n",
                code.chars().take(50).collect::<String>()
            ));
        }
    }

    fn rt_benchmark(&mut self) {
        let idx = self.pop() as usize;
        if idx < self.code_strings.len() {
            let code = self.code_strings[idx].clone();
            self.fitness.benchmark_code = Some(code.clone());
            self.emit_str(&format!(
                "benchmark set: {}\n",
                code.chars().take(50).collect::<String>()
            ));
        }
    }

    fn prim_trust(&mut self) {
        // Expect a node ID on the stack (as a number).
        let id_val = self.pop() as u64;
        let id_bytes = id_val.to_be_bytes();
        self.trusted_peers.insert(id_bytes);
        self.emit_str(&format!("trusted: {:016x}\n", id_val));
    }

    /// Run one evolution cycle.
    fn do_evolve(&mut self) {
        // Get mesh average fitness.
        let avg_fitness = self
            .mesh
            .as_ref()
            .map(|m| {
                let peers = m.peer_fitness_list();
                if peers.is_empty() {
                    self.fitness.score
                } else {
                    let total: i64 =
                        peers.iter().map(|p| p.score).sum::<i64>() + self.fitness.score;
                    total / (peers.len() as i64 + 1)
                }
            })
            .unwrap_or(self.fitness.score);

        // Run benchmark before mutation.
        let before_score = self.run_benchmark();

        // Apply a random mutation.
        let mutable_indices: Vec<usize> = self
            .dictionary
            .iter()
            .enumerate()
            .filter(|(_, e)| mutation::is_mutable(e))
            .map(|(i, _)| i)
            .collect();
        if mutable_indices.is_empty() {
            self.emit_str("evolve: no mutable words\n");
            return;
        }
        let idx = mutable_indices[self.rng.next_usize(mutable_indices.len())];
        let dict_len = self.dictionary.len();
        if let Some(mut record) =
            mutation::mutate_entry(&mut self.dictionary[idx], &mut self.rng, dict_len)
        {
            record.word_index = idx;

            // Run benchmark after mutation.
            let after_score = self.run_benchmark();

            if after_score >= before_score {
                self.emit_str(&format!(
                    "evolve: kept mutation ({} -> {}): {}\n",
                    before_score,
                    after_score,
                    record.format()
                ));
                self.mutation_history.push(record);
            } else {
                mutation::undo_mutation(&mut self.dictionary[idx], &record);
                self.emit_str(&format!(
                    "evolve: reverted mutation ({} -> {})\n",
                    before_score, after_score
                ));
            }
        } else {
            self.emit_str("evolve: mutation failed\n");
        }
        self.fitness.mark_evolved();
        self.emit_str(&format!(
            "evolve: own={} avg={} evolutions={}\n",
            self.fitness.score, avg_fitness, self.fitness.evolution_count
        ));
    }

    /// Run the benchmark code and return a score (stack depth after execution).
    fn run_benchmark(&mut self) -> i64 {
        let code = match self.fitness.benchmark_code.clone() {
            Some(c) => c,
            None => return 0,
        };
        let start = Instant::now();
        let result = self.execute_sandbox(&code);
        let elapsed = start.elapsed().as_millis() as i64;
        // Score = stack depth * 10 - elapsed_ms (reward correct output, penalize slowness).
        let depth_score = result.stack_snapshot.len() as i64 * 10;
        let time_penalty = (elapsed / 100).min(50);
        if result.success {
            depth_score - time_penalty
        } else {
            -100
        }
    }

    fn check_auto_evolve(&mut self) {
        if self.fitness.should_auto_evolve() {
            self.do_evolve();
        }
    }

    // -----------------------------------------------------------------------
    // WebSocket bridge primitives
    // -----------------------------------------------------------------------

    fn prim_ws_status(&mut self) {
        if let Some(ref st) = self.ws_state {
            let s = st.lock().unwrap().format_status();
            self.emit_str(&s);
        } else {
            self.emit_str("ws-bridge: not running\n");
        }
    }

    fn prim_ws_clients(&mut self) {
        if let Some(ref st) = self.ws_state {
            let s = st.lock().unwrap().format_clients();
            self.emit_str(&s);
        } else {
            self.emit_str("  (ws-bridge not running)\n");
        }
    }

    fn prim_ws_broadcast(&mut self) {
        let msg = self.parse_until('"');
        // The broadcast happens by updating the mesh_json which gets
        // pushed to all connected browsers on the next 2s tick.
        if let Ok(mut json) = self.ws_mesh_json.lock() {
            *json = format!(
                r#"{{"type":"broadcast","message":"{}"}}"#,
                msg.replace('"', "\\\"")
            );
        }
        self.emit_str(&format!("ws broadcast: {}\n", msg));
    }

    fn update_ws_mesh_json(&mut self) {
        let id_hex = self
            .node_id_cache
            .map(|id| mesh::id_to_hex(&id))
            .unwrap_or_default();
        let peer_details = self
            .mesh
            .as_ref()
            .map(|m| m.peer_details())
            .unwrap_or_default();
        let goal_stats = self
            .mesh
            .as_ref()
            .map(|m| m.goal_stats())
            .unwrap_or((0, 0, 0, 0));
        let recent = self
            .mesh
            .as_ref()
            .map(|m| m.drain_recent_events())
            .unwrap_or_default();
        let children: Vec<(String, u32)> = self
            .spawn_state
            .children
            .iter()
            .map(|c| (mesh::id_to_hex(&c.node_id), self.spawn_state.generation + 1))
            .collect();
        let json = ws_bridge::build_mesh_json(ws_bridge::MeshJsonParams {
            self_id: &id_hex,
            self_fitness: self.fitness.score,
            self_generation: self.spawn_state.generation,
            peers: &peer_details,
            goals: goal_stats,
            recent_events: &recent,
            children: &children,
            watch_count: self.monitor.watches.len(),
            alert_count: self.monitor.alerts.len(),
        });
        if let Ok(mut j) = self.ws_mesh_json.lock() {
            *j = json;
        }
    }

    fn poll_ws_events(&mut self) {
        // Process incoming WS events (goal submissions from browsers).
        let events: Vec<ws_bridge::WsEvent> = self
            .ws_events
            .as_ref()
            .map(|rx| {
                let mut evts = Vec::new();
                while let Ok(e) = rx.try_recv() {
                    evts.push(e);
                }
                evts
            })
            .unwrap_or_default();

        for event in events {
            match event {
                ws_bridge::WsEvent::GoalSubmit { code, priority } => {
                    if let Some(ref m) = self.mesh {
                        let gid = m.create_goal(&code, priority, Some(code.clone()));
                        println!(
                            "[ws] goal #{} from browser: {}",
                            gid,
                            code.chars().take(40).collect::<String>()
                        );
                    }
                }
                ws_bridge::WsEvent::ClientConnected { id } => {
                    println!("[ws] browser connected: {}", id);
                }
                ws_bridge::WsEvent::ClientDisconnected { id } => {
                    println!("[ws] browser disconnected: {}", id);
                }
                ws_bridge::WsEvent::Heartbeat { .. } => {}
            }
        }
    }

    // -----------------------------------------------------------------------
    // Monitoring & Ops primitives
    // -----------------------------------------------------------------------

    fn prim_watch(&mut self, kind: i32) {
        let target = self.parse_until('"');
        let rt_prim = match kind {
            0 => P_WATCH_URL_RT,
            1 => P_WATCH_FILE_RT,
            2 => P_WATCH_PROC_RT,
            _ => P_WATCH_URL_RT,
        };
        if self.compiling {
            let idx = self.code_strings.len();
            self.code_strings.push(target);
            if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::Literal(idx as Cell));
                def.body.push(Instruction::Primitive(rt_prim));
            }
        } else {
            self.do_add_watch(kind, &target);
        }
    }

    fn rt_watch(&mut self, kind: i32) {
        let idx = self.pop() as usize;
        if idx < self.code_strings.len() {
            let target = self.code_strings[idx].clone();
            self.do_add_watch(kind, &target);
        }
    }

    fn do_add_watch(&mut self, kind: i32, target: &str) {
        let interval = self.pop() as u64;
        let wk = match kind {
            0 => monitor::WatchKind::Url(target.to_string()),
            1 => monitor::WatchKind::File(target.to_string()),
            2 => monitor::WatchKind::Process(target.to_string()),
            _ => return,
        };
        let id = self.monitor.add_watch(wk, interval.max(1));
        self.stack.push(id as Cell);
        self.emit_str(&format!("watch #{} created (every {}s)\n", id, interval));
    }

    fn prim_on_alert(&mut self) {
        let code = self.parse_until('"');
        if self.compiling {
            let idx = self.code_strings.len();
            self.code_strings.push(code);
            if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::Primitive(P_ON_ALERT_RT));
                def.body.push(Instruction::Literal(idx as Cell));
            }
        } else {
            let watch_id = self.pop() as u32;
            self.monitor.set_alert_handler(watch_id, code);
            self.emit_str(&format!("alert handler set for watch #{}\n", watch_id));
        }
    }

    fn rt_on_alert(&mut self) {
        let idx = self.pop() as usize;
        let watch_id = self.pop() as u32;
        if idx < self.code_strings.len() {
            let code = self.code_strings[idx].clone();
            self.monitor.set_alert_handler(watch_id, code);
        }
    }

    fn prim_alert_threshold(&mut self) {
        let target = self.parse_until('"');
        if self.compiling {
            let idx = self.code_strings.len();
            self.code_strings.push(target);
            if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::Literal(idx as Cell));
                def.body.push(Instruction::Primitive(P_ALERT_THRESHOLD_RT));
            }
        } else if let Ok(watch_id) = target.trim().parse::<u32>() {
            let level = self.pop();
            self.monitor
                .set_alert_level(watch_id, monitor::AlertLevel::from_val(level));
            self.emit_str(&format!("alert threshold set for watch #{}\n", watch_id));
        }
    }

    fn rt_alert_threshold(&mut self) {
        let idx = self.pop() as usize;
        if idx < self.code_strings.len() {
            if let Ok(watch_id) = self.code_strings[idx].trim().parse::<u32>() {
                let level = self.pop();
                self.monitor
                    .set_alert_level(watch_id, monitor::AlertLevel::from_val(level));
            }
        }
    }

    fn prim_dashboard(&mut self) {
        let peer_count = self.mesh.as_ref().map(|m| m.peer_count()).unwrap_or(0);
        let goal_summary = self
            .mesh
            .as_ref()
            .map(|m| m.format_goals())
            .unwrap_or_default();
        let s = self
            .monitor
            .format_dashboard(peer_count, self.fitness.score, &goal_summary);
        self.emit_str(&s);
    }

    fn prim_health(&mut self) {
        let peer_count = self.mesh.as_ref().map(|m| m.peer_count()).unwrap_or(0);
        let score = self.monitor.health_score(peer_count, self.fitness.score);
        self.stack.push(score);
    }

    fn prim_every(&mut self) {
        let interval = self.pop() as u64;
        // Consume the rest of the input line as the code to schedule.
        let remaining = self.input_buffer[self.input_pos..].trim().to_string();
        self.input_pos = self.input_buffer.len(); // consume it
        if remaining.is_empty() {
            self.emit_str("EVERY: no code to schedule\n");
            return;
        }
        let id = self
            .monitor
            .add_schedule(remaining.clone(), interval.max(1));
        self.stack.push(id as Cell);
        self.emit_str(&format!(
            "schedule #{} every {}s: {}\n",
            id,
            interval,
            remaining.chars().take(40).collect::<String>()
        ));
    }

    fn rt_every(&mut self) {
        // For compiled EVERY, not yet supported — would need code string storage.
        self.emit_str("EVERY only works at the REPL\n");
    }

    fn prim_heal(&mut self) {
        self.emit_str("--- heal cycle ---\n");
        // Check all watches.
        let due = self.monitor.due_watches();
        if due.is_empty() {
            self.emit_str("  no watches due\n");
        }
        for wid in &due {
            self.run_watch_check(*wid);
        }
        // Run handlers for active alerts.
        let handlers: Vec<(u32, String)> = self
            .monitor
            .alerts
            .iter()
            .filter(|a| !a.acknowledged)
            .filter_map(|a| {
                self.monitor
                    .watches
                    .get(&a.watch_id)
                    .and_then(|w| w.alert_handler.clone())
                    .map(|h| (a.id, h))
            })
            .collect();
        for (aid, handler) in &handlers {
            self.emit_str(&format!("  running handler for alert #{}\n", aid));
            self.interpret_line(handler);
        }
        self.emit_str("--- heal done ---\n");
    }

    /// Execute a watch check for a specific watch ID.
    fn run_watch_check(&mut self, watch_id: u32) {
        let kind = match self.monitor.watches.get(&watch_id) {
            Some(w) => w.kind.clone(),
            None => return,
        };
        let start = Instant::now();
        let status = match kind {
            monitor::WatchKind::Url(ref url) => match io_words::http_get(url) {
                Ok((_, code)) => {
                    let ms = start.elapsed().as_millis() as u64;
                    if (200..400).contains(&code) {
                        monitor::WatchStatus::up(code as i32, ms, format!("{}", code))
                    } else {
                        monitor::WatchStatus::down(code as i32, format!("HTTP {}", code))
                    }
                }
                Err(e) => monitor::WatchStatus::down(-1, e),
            },
            monitor::WatchKind::File(ref path) => {
                if io_words::file_exists(path) {
                    let ms = start.elapsed().as_millis() as u64;
                    match std::fs::metadata(path) {
                        Ok(m) => monitor::WatchStatus::up(0, ms, format!("{}b", m.len())),
                        Err(e) => monitor::WatchStatus::down(-1, e.to_string()),
                    }
                } else {
                    monitor::WatchStatus::down(-1, "not found".into())
                }
            }
            monitor::WatchKind::Process(ref name) => {
                match io_words::shell_exec(&format!(
                    "pgrep -x {} >/dev/null 2>&1 && echo UP || echo DOWN",
                    name
                )) {
                    Ok((stdout, _)) => {
                        let ms = start.elapsed().as_millis() as u64;
                        let out = String::from_utf8_lossy(&stdout).trim().to_string();
                        if out.contains("UP") {
                            monitor::WatchStatus::up(0, ms, "running".into())
                        } else {
                            monitor::WatchStatus::down(-1, "not running".into())
                        }
                    }
                    Err(e) => monitor::WatchStatus::down(-1, e),
                }
            }
        };

        // Record the check result.
        if let Some(alert) = self.monitor.record_check(watch_id, status.clone()) {
            self.emit_str(&format!(
                "ALERT [{}] watch #{}: {}\n",
                alert.level.label(),
                watch_id,
                alert.message
            ));
            // Run alert handler if defined.
            let handler = self
                .monitor
                .watches
                .get(&watch_id)
                .and_then(|w| w.alert_handler.clone());
            if let Some(code) = handler {
                self.interpret_line(&code);
                // Fitness bonus for attempted remediation.
                self.fitness.score += 15;
            }
        }
    }

    /// Tick the monitor: check due watches and run due schedules.
    fn tick_monitor(&mut self) {
        // Check due watches.
        let due_watches = self.monitor.due_watches();
        for wid in due_watches {
            self.run_watch_check(wid);
        }

        // Run due schedules.
        let due_scheds = self.monitor.due_schedules();
        for (_sid, code) in due_scheds {
            self.interpret_line(&code);
        }
    }

    // -----------------------------------------------------------------------
    // Spawn / Replication primitives
    // -----------------------------------------------------------------------

    fn build_state_for_spawn(&self) -> Vec<u8> {
        let snap = self.make_snapshot();
        persist::serialize_snapshot(&snap)
    }

    fn prim_spawn(&mut self) {
        if let Err(e) = self.spawn_state.can_spawn() {
            self.emit_str(&format!("SPAWN: {}\n", e));
            return;
        }
        let state = self.build_state_for_spawn();
        let package = match spawn::build_package(&state) {
            Ok(p) => p,
            Err(e) => {
                self.emit_str(&format!("SPAWN: {}\n", e));
                return;
            }
        };
        let parent_port = self.mesh.as_ref().map(|m| m.local_port()).unwrap_or(0);
        let child_gen = self.spawn_state.generation + 1;

        match spawn::spawn_local(&package, parent_port, child_gen) {
            Ok((pid, port, child_id)) => {
                self.spawn_state.children.push(spawn::ChildInfo {
                    pid,
                    port,
                    node_id: child_id,
                    spawned_at: Instant::now(),
                });
                self.spawn_state.last_spawn = Some(Instant::now());
                self.emit_str(&format!(
                    "spawned child pid={} id={}\n",
                    pid,
                    mesh::id_to_hex(&child_id)
                ));
            }
            Err(e) => self.emit_str(&format!("SPAWN: {}\n", e)),
        }
    }

    fn prim_spawn_n(&mut self) {
        let n = self.pop() as usize;
        for i in 0..n {
            self.prim_spawn();
            // Override cooldown for batch spawns.
            if i < n - 1 {
                self.spawn_state.last_spawn = None;
            }
        }
    }

    fn prim_package(&mut self) {
        let state = self.build_state_for_spawn();
        match spawn::build_package(&state) {
            Ok(pkg) => {
                let len = pkg.len().min(self.memory.len() - PAD);
                for (i, &byte) in pkg.iter().take(len).enumerate() {
                    self.memory[PAD + i] = byte as Cell;
                }
                self.stack.push(PAD as Cell);
                self.stack.push(len as Cell);
                self.emit_str(&format!("package: {} bytes\n", pkg.len()));
            }
            Err(e) => {
                self.emit_str(&format!("PACKAGE: {}\n", e));
                self.stack.push(0);
                self.stack.push(0);
            }
        }
    }

    fn prim_package_size(&mut self) {
        let state = self.build_state_for_spawn();
        match spawn::package_size_estimate(state.len()) {
            Ok(size) => {
                self.stack.push(size as Cell);
                self.emit_str(&format!("package size: {} bytes\n", size));
            }
            Err(e) => {
                self.emit_str(&format!("PACKAGE-SIZE: {}\n", e));
                self.stack.push(0);
            }
        }
    }

    fn prim_children(&mut self) {
        if self.spawn_state.children.is_empty() {
            self.emit_str("  (no children)\n");
        } else {
            let lines: Vec<String> = self
                .spawn_state
                .children
                .iter()
                .map(|c| {
                    format!(
                        "  pid={} id={} age={}s\n",
                        c.pid,
                        mesh::id_to_hex(&c.node_id),
                        c.spawned_at.elapsed().as_secs()
                    )
                })
                .collect();
            for line in &lines {
                self.emit_str(line);
            }
        }
    }

    fn prim_family(&mut self) {
        let self_id = self
            .node_id_cache
            .map(|id| mesh::id_to_hex(&id))
            .unwrap_or_else(|| "?".to_string());
        let parent = self
            .spawn_state
            .parent_id
            .map(|id| mesh::id_to_hex(&id))
            .unwrap_or_else(|| "none".to_string());
        self.emit_str(&format!(
            "id: {} gen: {} parent: {} children: {}\n",
            self_id,
            self.spawn_state.generation,
            parent,
            self.spawn_state.children.len(),
        ));
    }

    fn prim_kill_child(&mut self) {
        let pid = self.pop() as u32;
        #[cfg(unix)]
        {
            unsafe {
                libc_kill(pid as i32, 15); // SIGTERM
            }
        }
        self.spawn_state.children.retain(|c| c.pid != pid);
        self.emit_str(&format!("sent SIGTERM to pid {}\n", pid));
    }

    fn prim_replicate_to(&mut self) {
        let addr = self.parse_until('"');
        if self.compiling {
            let idx = self.code_strings.len();
            self.code_strings.push(addr);
            if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::Literal(idx as Cell));
                def.body.push(Instruction::Primitive(P_REPLICATE_TO));
            }
            return;
        }
        let state = self.build_state_for_spawn();
        let package = match spawn::build_package(&state) {
            Ok(p) => p,
            Err(e) => {
                self.emit_str(&format!("REPLICATE-TO: {}\n", e));
                return;
            }
        };
        match spawn::send_package(&addr, &package) {
            Ok(()) => self.emit_str(&format!("sent {} bytes to {}\n", package.len(), addr)),
            Err(e) => self.emit_str(&format!("REPLICATE-TO: {}\n", e)),
        }
    }

    /// Check for and handle incoming replication packages.
    fn check_incoming_replications(&mut self) {
        if self.spawn_state.quarantine || !self.spawn_state.accept_replicate {
            return;
        }
        let pkg = self.mesh.as_ref().and_then(|m| m.recv_replication());
        if let Some(pkg) = pkg {
            let parent_port = self.mesh.as_ref().map(|m| m.local_port()).unwrap_or(0);
            let child_gen = self.spawn_state.generation + 1;
            match spawn::spawn_local(&pkg, parent_port, child_gen) {
                Ok((pid, _, child_id)) => {
                    self.spawn_state.children.push(spawn::ChildInfo {
                        pid,
                        port: 0,
                        node_id: child_id,
                        spawned_at: Instant::now(),
                    });
                    println!(
                        "[repl] spawned child pid={} id={}",
                        pid,
                        mesh::id_to_hex(&child_id)
                    );
                }
                Err(e) => eprintln!("[repl] spawn failed: {}", e),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Identity
    // -----------------------------------------------------------------------

    /// REIDENTIFY ( -- ) generate a new node ID, migrate saved state.
    fn prim_reidentify(&mut self) {
        let old_id = self.node_id_cache;
        // Generate a new random ID.
        let mut new_id = [0u8; 8];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(&mut new_id);
        }
        // Migrate state directory.
        if let Some(oid) = old_id {
            let _ = persist::rename_state(&oid, &new_id);
        }
        // Save the new ID.
        let _ = persist::save_node_id(&new_id);
        self.node_id_cache = Some(new_id);
        self.rng = mutation::SimpleRng::new(u64::from_be_bytes(new_id));
        self.emit_str(&format!(
            "reidentified: {} -> {}\n",
            old_id
                .map(|id| mesh::id_to_hex(&id))
                .unwrap_or_else(|| "none".into()),
            mesh::id_to_hex(&new_id),
        ));
    }

    // -----------------------------------------------------------------------
    // Persistence primitives
    // -----------------------------------------------------------------------

    fn make_snapshot(&self) -> persist::VmSnapshot {
        let node_id = self.node_id_cache.unwrap_or([0u8; 8]);
        let goals = self
            .mesh
            .as_ref()
            .map(|m| m.clone_goals())
            .unwrap_or_else(goals::GoalRegistry::empty);
        persist::VmSnapshot {
            node_id,
            dictionary: self.dictionary.clone(),
            memory: self.memory.clone(),
            here: self.here,
            goals,
            fitness: self.fitness.clone(),
            code_strings: self.code_strings.clone(),
        }
    }

    fn prim_save(&mut self) {
        if let Some(id) = self.node_id_cache {
            let snap = self.make_snapshot();
            let data = persist::serialize_snapshot(&snap);
            match persist::save_state(&id, &data) {
                Ok(()) => self.emit_str(&format!(
                    "saved {} bytes to {}\n",
                    data.len(),
                    persist::state_dir(&id)
                )),
                Err(e) => self.emit_str(&format!("save failed: {}\n", e)),
            }
        } else {
            self.emit_str("save: no node ID (mesh offline)\n");
        }
    }

    fn prim_load_state(&mut self) {
        if let Some(id) = self.node_id_cache {
            if let Some(data) = persist::load_state(&id) {
                if let Some(snap) = persist::deserialize_snapshot(&data) {
                    self.restore_snapshot(snap);
                    self.emit_str("state restored\n");
                } else {
                    self.emit_str("load: corrupt state file\n");
                }
            } else {
                self.emit_str("load: no saved state\n");
            }
        } else {
            self.emit_str("load: no node ID\n");
        }
    }

    fn prim_auto_save(&mut self) {
        self.auto_save_enabled = !self.auto_save_enabled;
        self.emit_str(&format!(
            "auto-save: {} (every {} tasks)\n",
            if self.auto_save_enabled { "ON" } else { "OFF" },
            self.auto_save_interval
        ));
    }

    fn prim_reset(&mut self) {
        if let Some(id) = self.node_id_cache {
            let _ = persist::delete_state(&id);
        }
        let _ = persist::delete_node_id();
        self.emit_str("state and identity deleted — restart for fresh boot\n");
    }

    fn prim_snapshots(&mut self) {
        if let Some(id) = self.node_id_cache {
            let snaps = persist::list_snapshots(&id);
            if snaps.is_empty() {
                self.emit_str("  (no snapshots)\n");
            } else {
                for name in &snaps {
                    self.emit_str(&format!("  {}\n", name));
                }
            }
        }
    }

    fn prim_snapshot(&mut self) {
        if let Some(id) = self.node_id_cache {
            let snap = self.make_snapshot();
            let data = persist::serialize_snapshot(&snap);
            match persist::save_snapshot(&id, &data) {
                Ok(name) => self.emit_str(&format!("snapshot: {}\n", name)),
                Err(e) => self.emit_str(&format!("snapshot failed: {}\n", e)),
            }
        }
    }

    fn prim_restore(&mut self) {
        let snap_id = self.pop();
        if let Some(id) = self.node_id_cache {
            let name = format!("{}", snap_id);
            if let Some(data) = persist::load_snapshot(&id, &name) {
                if let Some(snap) = persist::deserialize_snapshot(&data) {
                    self.restore_snapshot(snap);
                    self.emit_str(&format!("restored snapshot {}\n", name));
                } else {
                    self.emit_str("restore: corrupt snapshot\n");
                }
            } else {
                self.emit_str(&format!("snapshot {} not found\n", name));
            }
        }
    }

    fn restore_snapshot(&mut self, snap: persist::VmSnapshot) {
        self.dictionary = snap.dictionary;
        self.memory = snap.memory;
        self.here = snap.here;
        self.fitness = snap.fitness;
        self.code_strings = snap.code_strings;
        // Restore goals into mesh state if available.
        if let Some(ref m) = self.mesh {
            let mut st = m.state_lock();
            st.goals = snap.goals;
        }
    }

    fn check_auto_save(&mut self) {
        if !self.auto_save_enabled {
            return;
        }
        self.tasks_since_save += 1;
        if self.tasks_since_save >= self.auto_save_interval {
            self.tasks_since_save = 0;
            if let Some(id) = self.node_id_cache {
                let snap = self.make_snapshot();
                let data = persist::serialize_snapshot(&snap);
                let _ = persist::save_state(&id, &data);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Task decomposition primitives
    // -----------------------------------------------------------------------

    /// SUBTASK{ <code> } ( goal-id -- task-id ) add a subtask to a goal.
    fn prim_subtask(&mut self) {
        let code = self.parse_balanced_braces();
        if self.compiling {
            let idx = self.code_strings.len();
            self.code_strings.push(code);
            if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::Literal(idx as Cell));
                def.body.push(Instruction::Primitive(P_SUBTASK));
            }
        } else {
            let goal_id = self.pop() as u64;
            let result = self.mesh.as_ref().and_then(|m| {
                let mut st = m.state_lock();
                st.goals
                    .create_subtask(goal_id, code.clone(), Some(code.clone()))
            });
            if let Some(tid) = result {
                self.emit_str(&format!("subtask #{} added to goal #{}\n", tid, goal_id));
                self.stack.push(tid as Cell);
            } else {
                self.emit_str(&format!("goal #{} not found\n", goal_id));
                self.stack.push(0);
            }
        }
    }

    /// FORK ( goal-id n -- ) split an existing goal into n tasks.
    fn prim_fork(&mut self) {
        let n = self.pop() as usize;
        let goal_id = self.pop() as u64;
        let ok = self.mesh.as_ref().is_some_and(|m| {
            let mut st = m.state_lock();
            st.goals.fork_goal(goal_id, n)
        });
        if ok {
            self.emit_str(&format!("goal #{} forked into {} tasks\n", goal_id, n));
        } else {
            self.emit_str(&format!(
                "fork failed: goal #{} not found or no code\n",
                goal_id
            ));
        }
    }

    /// RESULTS ( goal-id -- ) show all subtask results.
    fn prim_results(&mut self) {
        let goal_id = self.pop() as u64;
        let out = if let Some(ref m) = self.mesh {
            let st = m.state_lock();
            let results = st.goals.collect_results(goal_id);
            if results.is_empty() {
                format!("goal #{}: no results\n", goal_id)
            } else {
                let mut s = format!("goal #{}: {} results\n", goal_id, results.len());
                for (tid, result) in &results {
                    s.push_str(&format!("  task #{}:", tid));
                    if let Some(r) = result {
                        if !r.stack_snapshot.is_empty() {
                            s.push_str(" stack=");
                            for v in &r.stack_snapshot {
                                s.push_str(&format!("{} ", v));
                            }
                        }
                        if !r.output.is_empty() {
                            s.push_str(&format!(" output=\"{}\"", r.output.trim_end()));
                        }
                        s.push('\n');
                    } else {
                        s.push_str(" (pending)\n");
                    }
                }
                s
            }
        } else {
            "mesh offline\n".to_string()
        };
        self.emit_str(&out);
    }

    /// REDUCE" <forth code>" ( goal-id -- ) apply reduction across subtask results.
    fn prim_reduce(&mut self) {
        let code = self.parse_until('"');
        if self.compiling {
            let idx = self.code_strings.len();
            self.code_strings.push(code);
            if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::Literal(idx as Cell));
                def.body.push(Instruction::Primitive(P_REDUCE_RT));
            }
        } else {
            self.do_reduce(&code);
        }
    }

    fn rt_reduce(&mut self) {
        let idx = self.pop() as usize;
        if idx < self.code_strings.len() {
            let code = self.code_strings[idx].clone();
            self.do_reduce(&code);
        }
    }

    fn do_reduce(&mut self, reduce_code: &str) {
        let goal_id = self.pop() as u64;
        // Collect all stack results from completed subtasks.
        let values: Vec<Cell> = if let Some(ref m) = self.mesh {
            let st = m.state_lock();
            let results = st.goals.collect_results(goal_id);
            results
                .iter()
                .filter_map(|(_, r)| r.as_ref())
                .flat_map(|r| r.stack_snapshot.iter().copied())
                .collect()
        } else {
            vec![]
        };

        if values.is_empty() {
            self.emit_str("reduce: no values to reduce\n");
            return;
        }

        // Push first value, then for each subsequent value push it and run reduce_code.
        self.stack.push(values[0]);
        for &val in &values[1..] {
            self.stack.push(val);
            self.interpret_line(reduce_code);
        }
        let result = self.stack.last().copied().unwrap_or(0);
        self.emit_str(&format!("reduce: {} values -> {}\n", values.len(), result));
    }

    /// PROGRESS ( goal-id -- ) show completion progress.
    fn prim_progress(&mut self) {
        let goal_id = self.pop() as u64;
        if let Some(ref m) = self.mesh {
            let st = m.state_lock();
            let s = st.goals.format_progress(goal_id);
            drop(st);
            self.emit_str(&s);
        }
    }

    // -----------------------------------------------------------------------
    // (load_prelude is defined in vm/compiler.rs)

    // -----------------------------------------------------------------------
    // REPL
    // -----------------------------------------------------------------------
}

// ===========================================================================
// REPL
// ===========================================================================

impl VM {
    fn repl(&mut self) {
        let stdin = io::stdin();
        let mut stdout = io::stdout();

        let _ = write!(stdout, "> ");
        let _ = stdout.flush();

        for line in stdin.lock().lines() {
            match line {
                Ok(line) => {
                    self.interpret_line(&line);
                    if !self.running {
                        break;
                    }
                    if !self.compiling {
                        self.check_auto_claim();
                        self.check_auto_replicate();
                        self.check_auto_evolve();
                        self.check_incoming_replications();
                        self.tick_monitor();
                        self.tick_swarm();
                        self.poll_ws_events();
                        self.update_ws_mesh_json();
                    }
                    if self.compiling {
                        let _ = write!(stdout, "  ");
                    } else {
                        let _ = write!(stdout, " ok\n> ");
                    }
                    let _ = stdout.flush();
                }
                Err(_) => break,
            }
        }
        println!();
    }
}

// ===========================================================================
// CLI argument parsing
// ===========================================================================

const VERSION: &str = "unit v0.15.2";

fn print_help() {
    println!("{}", VERSION);
    println!("A self-replicating software nanobot.\n");
    println!("USAGE:");
    println!("  unit                        Start interactive REPL");
    println!("  unit --eval \"2 3 + .\"       Evaluate and print result");
    println!("  unit --port 4201 --swarm    Start swarm node on port 4201");
    println!("  unit --file script.fs       Load a Forth script\n");
    println!("OPTIONS:");
    println!("  -h, --help                  Show this help");
    println!("  -v, --version               Print version and exit");
    println!("  -q, --quiet                 Suppress boot banner");
    println!("  --port PORT                 Set mesh UDP port (or UNIT_PORT env)");
    println!("  --peers HOST:PORT,...       Set seed peers (or UNIT_PEERS env)");
    println!("  --ws-port PORT             Set WebSocket bridge port");
    println!("  --eval \"FORTH CODE\"         Evaluate code, print output, exit");
    println!("  --file PATH                Load a .fs file, then start REPL");
    println!("  --no-mesh                  Start without mesh networking");
    println!("  --no-prelude               Start without loading prelude.fs");
    println!("  --swarm                    Start with SWARM-ON");
    println!("  --trust LEVEL              Set trust: all, mesh, family, none");
}

struct CliArgs {
    port: Option<u16>,
    peers: Option<String>,
    ws_port: Option<u16>,
    eval_code: Option<String>,
    file_path: Option<String>,
    no_mesh: bool,
    no_prelude: bool,
    swarm: bool,
    trust: Option<String>,
    quiet: bool,
}

fn parse_args() -> Option<CliArgs> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cli = CliArgs {
        port: None,
        peers: None,
        ws_port: None,
        eval_code: None,
        file_path: None,
        no_mesh: false,
        no_prelude: false,
        swarm: false,
        trust: None,
        quiet: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-v" | "--version" => {
                println!("{}", VERSION);
                std::process::exit(0);
            }
            "-q" | "--quiet" => cli.quiet = true,
            "--port" => {
                i += 1;
                cli.port = args.get(i).and_then(|s| s.parse().ok());
            }
            "--peers" => {
                i += 1;
                cli.peers = args.get(i).cloned();
            }
            "--ws-port" => {
                i += 1;
                cli.ws_port = args.get(i).and_then(|s| s.parse().ok());
            }
            "--eval" => {
                i += 1;
                cli.eval_code = args.get(i).cloned();
            }
            "--file" => {
                i += 1;
                cli.file_path = args.get(i).cloned();
            }
            "--no-mesh" => cli.no_mesh = true,
            "--no-prelude" => cli.no_prelude = true,
            "--swarm" => cli.swarm = true,
            "--trust" => {
                i += 1;
                cli.trust = args.get(i).cloned();
            }
            other => {
                eprintln!("unknown option: {}", other);
                std::process::exit(1);
            }
        }
        i += 1;
    }
    Some(cli)
}

// ===========================================================================
// Entry point
// ===========================================================================

fn main() {
    let cli = parse_args().unwrap();
    let mut vm = VM::new();
    vm.silent = cli.quiet;

    // Port: CLI flag > env var > default 0.
    let port: u16 = cli
        .port
        .or_else(|| std::env::var("UNIT_PORT").ok().and_then(|s| s.parse().ok()))
        .unwrap_or(0);

    let peers_str = cli
        .peers
        .or_else(|| std::env::var("UNIT_PEERS").ok())
        .unwrap_or_default();
    let seed_peers: Vec<SocketAddr> = peers_str
        .split(',')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.trim().parse().ok())
        .collect();

    // Start mesh unless --no-mesh.
    if !cli.no_mesh {
        let env_node_id: Option<[u8; 8]> = std::env::var("UNIT_NODE_ID").ok().and_then(|hex| {
            if hex.len() != 16 {
                return None;
            }
            let mut id = [0u8; 8];
            for i in 0..8 {
                id[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
            }
            Some(id)
        });

        let persisted_id = env_node_id.or_else(persist::load_node_id);
        let resumed = persisted_id.is_some() && env_node_id.is_none();

        match mesh::MeshNode::start_with_id(persisted_id, port, seed_peers) {
            Ok(node) => {
                let id = node.id_bytes();
                let seed = u64::from_be_bytes(id);
                vm.rng = mutation::SimpleRng::new(seed);
                vm.node_id_cache = Some(id);
                let _ = persist::save_node_id(&id);
                if resumed && !cli.quiet {
                    eprintln!("resumed identity {}", mesh::id_to_hex(&id));
                }
                vm.mesh = Some(node);

                let ws_port: u16 = cli
                    .ws_port
                    .or_else(|| {
                        std::env::var("UNIT_WS_PORT")
                            .ok()
                            .and_then(|s| s.parse().ok())
                    })
                    .unwrap_or_else(|| if port > 0 { port + 2000 } else { 0 });
                if ws_port > 0 {
                    match ws_bridge::start_ws_bridge(ws_port, vm.ws_mesh_json.clone()) {
                        Ok((ws_st, ws_rx)) => {
                            vm.ws_state = Some(ws_st);
                            vm.ws_events = Some(ws_rx);
                            if !cli.quiet {
                                eprintln!("ws-bridge: listening on port {}", ws_port);
                            }
                        }
                        Err(e) => {
                            if !cli.quiet {
                                eprintln!("ws-bridge: {}", e);
                            }
                        }
                    }
                }

                if let Ok(gen_str) = std::env::var("UNIT_GENERATION") {
                    if let Ok(gen) = gen_str.parse::<u32>() {
                        vm.spawn_state.generation = gen;
                    }
                }
                if let Ok(parent_hex) = std::env::var("UNIT_PARENT_ID") {
                    if parent_hex.len() == 16 {
                        let mut pid = [0u8; 8];
                        let mut ok = true;
                        for i in 0..8 {
                            match u8::from_str_radix(&parent_hex[i * 2..i * 2 + 2], 16) {
                                Ok(b) => pid[i] = b,
                                Err(_) => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        if ok {
                            vm.spawn_state.parent_id = Some(pid);
                        }
                    }
                }
            }
            Err(e) => {
                if !cli.quiet {
                    eprintln!("mesh: failed to start: {}", e);
                }
            }
        }
    }

    if let Some(ref m) = vm.mesh {
        m.set_load(vm.dictionary.len() as u32);
    }

    // Restore state or load prelude.
    let mut restored = false;
    if let Some(id) = vm.node_id_cache {
        if let Some(data) = persist::load_state(&id) {
            if let Some(snap) = persist::deserialize_snapshot(&data) {
                vm.dictionary = snap.dictionary;
                vm.memory = snap.memory;
                vm.here = snap.here;
                vm.fitness = snap.fitness;
                vm.code_strings = snap.code_strings;
                if let Some(ref m) = vm.mesh {
                    let mut st = m.state_lock();
                    st.goals = snap.goals;
                }
                restored = true;
                if !cli.quiet {
                    eprintln!("restored from {}/state.bin", persist::state_dir(&id));
                }
            }
        }
    }

    if !restored && !cli.no_prelude {
        // Suppress prelude output for --eval and --quiet modes.
        let suppress = cli.eval_code.is_some() || cli.quiet;
        if suppress {
            vm.output_buffer = Some(String::new());
        }
        vm.load_prelude();
        if suppress {
            vm.output_buffer = None;
        }
    }
    vm.silent = false;

    // Apply --trust.
    if let Some(ref level) = cli.trust {
        match level.as_str() {
            "all" => vm.interpret_line("TRUST-ALL"),
            "mesh" => vm.interpret_line("TRUST-MESH"),
            "family" => vm.interpret_line("TRUST-FAMILY"),
            "none" => vm.interpret_line("TRUST-NONE"),
            _ => eprintln!("unknown trust level: {}", level),
        }
    }

    // Apply --swarm.
    if cli.swarm {
        vm.interpret_line("SWARM-ON");
    }

    // --file: load a Forth script.
    if let Some(ref path) = cli.file_path {
        match std::fs::read_to_string(path) {
            Ok(source) => {
                for line in source.lines() {
                    vm.interpret_line(line);
                }
            }
            Err(e) => {
                eprintln!("error: cannot read {}: {}", path, e);
                std::process::exit(1);
            }
        }
    }

    // --eval: evaluate and exit.
    if let Some(ref code) = cli.eval_code {
        let output = vm.eval(code);
        if !output.is_empty() {
            print!("{}", output);
        }
        return;
    }

    vm.repl();
}
