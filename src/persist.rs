// persist.rs — State persistence for unit
//
// Serializes the full VM state to disk so a unit can be stopped and
// resumed. State is saved to ~/.unit/<node-id>/state.bin with optional
// timestamped snapshots.
//
// Format: binary, version-tagged. Uses the same wire helpers as mesh.rs.

#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

use crate::features::fitness::FitnessTracker;
use crate::goals::{Goal, GoalRegistry, GoalStatus, Task, TaskResult, TaskStatus};
use crate::mesh::NodeId;
use crate::types::{Cell, Entry, Instruction};

const PERSIST_MAGIC: &[u8; 4] = b"USAV";
const PERSIST_VERSION: u8 = 1;

// ---------------------------------------------------------------------------
// Wire format helpers (duplicated from mesh.rs to avoid coupling)
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
    let v = u32::from_be_bytes(data[*pos..*pos + 4].try_into().ok()?);
    *pos += 4;
    Some(v)
}
fn read_u64(data: &[u8], pos: &mut usize) -> Option<u64> {
    if *pos + 8 > data.len() {
        return None;
    }
    let v = u64::from_be_bytes(data[*pos..*pos + 8].try_into().ok()?);
    *pos += 8;
    Some(v)
}
fn read_i64(data: &[u8], pos: &mut usize) -> Option<i64> {
    if *pos + 8 > data.len() {
        return None;
    }
    let v = i64::from_be_bytes(data[*pos..*pos + 8].try_into().ok()?);
    *pos += 8;
    Some(v)
}
fn read_bytes(data: &[u8], pos: &mut usize, n: usize) -> Option<Vec<u8>> {
    if *pos + n > data.len() {
        return None;
    }
    let v = data[*pos..*pos + n].to_vec();
    *pos += n;
    Some(v)
}
fn read_string(data: &[u8], pos: &mut usize) -> Option<String> {
    let len = read_u16(data, pos)? as usize;
    let bytes = read_bytes(data, pos, len)?;
    Some(String::from_utf8_lossy(&bytes).to_string())
}
fn write_string(buf: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    write_u16(buf, b.len() as u16);
    write_bytes(buf, b);
}

// ---------------------------------------------------------------------------
// Instruction serialization
// ---------------------------------------------------------------------------

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
            write_string(buf, s);
        }
        Instruction::Branch(off) => {
            write_u8(buf, 4);
            write_i64(buf, *off);
        }
        Instruction::BranchIfZero(off) => {
            write_u8(buf, 5);
            write_i64(buf, *off);
        }
    }
}

fn deserialize_instruction(data: &[u8], pos: &mut usize) -> Option<Instruction> {
    let tag = read_u8(data, pos)?;
    match tag {
        0 => Some(Instruction::Primitive(read_u32(data, pos)? as usize)),
        1 => Some(Instruction::Literal(read_i64(data, pos)?)),
        2 => Some(Instruction::Call(read_u32(data, pos)? as usize)),
        3 => Some(Instruction::StringLit(read_string(data, pos)?)),
        4 => Some(Instruction::Branch(read_i64(data, pos)?)),
        5 => Some(Instruction::BranchIfZero(read_i64(data, pos)?)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Full VM state snapshot
// ---------------------------------------------------------------------------

pub struct VmSnapshot {
    pub node_id: NodeId,
    pub dictionary: Vec<Entry>,
    pub memory: Vec<Cell>,
    pub here: usize,
    pub goals: GoalRegistry,
    pub fitness: FitnessTracker,
    pub code_strings: Vec<String>,
}

pub fn serialize_snapshot(snap: &VmSnapshot) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8192);

    // Header.
    write_bytes(&mut buf, PERSIST_MAGIC);
    write_u8(&mut buf, PERSIST_VERSION);
    write_bytes(&mut buf, &snap.node_id);

    // Dictionary.
    write_u32(&mut buf, snap.dictionary.len() as u32);
    for entry in &snap.dictionary {
        write_string(&mut buf, &entry.name);
        let flags = (if entry.immediate { 1u8 } else { 0 }) | (if entry.hidden { 2u8 } else { 0 });
        write_u8(&mut buf, flags);
        write_u32(&mut buf, entry.body.len() as u32);
        for instr in &entry.body {
            serialize_instruction(&mut buf, instr);
        }
    }

    // Memory (only up to `here`).
    let mem_cells = snap.here.min(snap.memory.len());
    write_u32(&mut buf, snap.here as u32);
    write_u32(&mut buf, mem_cells as u32);
    for i in 0..mem_cells {
        write_i64(&mut buf, snap.memory[i]);
    }

    // Goals.
    let goal_list: Vec<&Goal> = snap.goals.goals.values().collect();
    write_u32(&mut buf, goal_list.len() as u32);
    for goal in &goal_list {
        write_u64(&mut buf, goal.id);
        write_string(&mut buf, &goal.description);
        match &goal.code {
            Some(c) => {
                write_u8(&mut buf, 1);
                write_string(&mut buf, c);
            }
            None => write_u8(&mut buf, 0),
        }
        write_i64(&mut buf, goal.priority);
        write_u8(&mut buf, goal.status.as_u8());
        write_bytes(&mut buf, &goal.creator);
        write_u64(&mut buf, goal.created_at);
        write_u16(&mut buf, goal.task_ids.len() as u16);
        for tid in &goal.task_ids {
            write_u64(&mut buf, *tid);
        }
    }

    // Tasks.
    let task_list: Vec<&Task> = snap.goals.tasks.values().collect();
    write_u32(&mut buf, task_list.len() as u32);
    for task in &task_list {
        write_u64(&mut buf, task.id);
        write_u64(&mut buf, task.goal_id);
        write_string(&mut buf, &task.description);
        write_u8(&mut buf, task.status.as_u8());
        match &task.assigned_to {
            Some(id) => {
                write_u8(&mut buf, 1);
                write_bytes(&mut buf, id);
            }
            None => write_u8(&mut buf, 0),
        }
        write_u64(&mut buf, task.created_at);
        match &task.result {
            Some(r) => {
                write_u8(&mut buf, 1);
                write_u8(&mut buf, if r.success { 1 } else { 0 });
                write_u16(&mut buf, r.stack_snapshot.len() as u16);
                for &v in &r.stack_snapshot {
                    write_i64(&mut buf, v);
                }
                write_string(&mut buf, &r.output);
                write_string(&mut buf, r.error.as_deref().unwrap_or(""));
            }
            None => write_u8(&mut buf, 0),
        }
    }

    // Fitness.
    write_i64(&mut buf, snap.fitness.score);
    write_u32(&mut buf, snap.fitness.tasks_completed);
    write_u32(&mut buf, snap.fitness.tasks_failed);
    write_u64(&mut buf, snap.fitness.total_time_ms);
    write_u32(&mut buf, snap.fitness.evolution_count);

    // Code strings.
    write_u32(&mut buf, snap.code_strings.len() as u32);
    for s in &snap.code_strings {
        write_string(&mut buf, s);
    }

    buf
}

pub fn deserialize_snapshot(data: &[u8]) -> Option<VmSnapshot> {
    let mut pos = 0;

    // Header.
    let magic = read_bytes(data, &mut pos, 4)?;
    if magic != PERSIST_MAGIC {
        return None;
    }
    let version = read_u8(data, &mut pos)?;
    if version != PERSIST_VERSION {
        return None;
    }
    let id_bytes = read_bytes(data, &mut pos, 8)?;
    let mut node_id = [0u8; 8];
    node_id.copy_from_slice(&id_bytes);

    // Dictionary.
    let dict_count = read_u32(data, &mut pos)? as usize;
    let mut dictionary = Vec::with_capacity(dict_count);
    for _ in 0..dict_count {
        let name = read_string(data, &mut pos)?;
        let flags = read_u8(data, &mut pos)?;
        let body_len = read_u32(data, &mut pos)? as usize;
        let mut body = Vec::with_capacity(body_len);
        for _ in 0..body_len {
            body.push(deserialize_instruction(data, &mut pos)?);
        }
        dictionary.push(Entry {
            name,
            immediate: flags & 1 != 0,
            hidden: flags & 2 != 0,
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

    // Goals.
    let mut goals = GoalRegistry::empty();
    let goal_count = read_u32(data, &mut pos)? as usize;
    for _ in 0..goal_count {
        let id = read_u64(data, &mut pos)?;
        let description = read_string(data, &mut pos)?;
        let has_code = read_u8(data, &mut pos)? != 0;
        let code = if has_code {
            Some(read_string(data, &mut pos)?)
        } else {
            None
        };
        let priority = read_i64(data, &mut pos)?;
        let status = GoalStatus::from_u8(read_u8(data, &mut pos)?);
        let creator_bytes = read_bytes(data, &mut pos, 8)?;
        let mut creator = [0u8; 8];
        creator.copy_from_slice(&creator_bytes);
        let created_at = read_u64(data, &mut pos)?;
        let task_count = read_u16(data, &mut pos)? as usize;
        let mut task_ids = Vec::with_capacity(task_count);
        for _ in 0..task_count {
            task_ids.push(read_u64(data, &mut pos)?);
        }
        goals.goals.insert(
            id,
            Goal {
                id,
                description,
                code,
                priority,
                status,
                created_at,
                creator,
                task_ids,
            },
        );
    }

    // Tasks.
    let task_count = read_u32(data, &mut pos)? as usize;
    for _ in 0..task_count {
        let id = read_u64(data, &mut pos)?;
        let goal_id = read_u64(data, &mut pos)?;
        let description = read_string(data, &mut pos)?;
        let status = TaskStatus::from_u8(read_u8(data, &mut pos)?);
        let has_assignee = read_u8(data, &mut pos)? != 0;
        let assigned_to = if has_assignee {
            let b = read_bytes(data, &mut pos, 8)?;
            let mut id = [0u8; 8];
            id.copy_from_slice(&b);
            Some(id)
        } else {
            None
        };
        let created_at = read_u64(data, &mut pos)?;
        let has_result = read_u8(data, &mut pos)? != 0;
        let result = if has_result {
            let success = read_u8(data, &mut pos)? != 0;
            let slen = read_u16(data, &mut pos)? as usize;
            let mut stack_snapshot = Vec::with_capacity(slen);
            for _ in 0..slen {
                stack_snapshot.push(read_i64(data, &mut pos)?);
            }
            let output = read_string(data, &mut pos)?;
            let err_str = read_string(data, &mut pos)?;
            let error = if err_str.is_empty() {
                None
            } else {
                Some(err_str)
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
        goals.tasks.insert(
            id,
            Task {
                id,
                goal_id,
                description,
                code: None,
                assigned_to,
                status,
                result,
                created_at,
            },
        );
    }

    // Fitness.
    let mut fitness = FitnessTracker::new();
    fitness.score = read_i64(data, &mut pos)?;
    fitness.tasks_completed = read_u32(data, &mut pos)?;
    fitness.tasks_failed = read_u32(data, &mut pos)?;
    fitness.total_time_ms = read_u64(data, &mut pos)?;
    fitness.evolution_count = read_u32(data, &mut pos)?;

    // Code strings.
    let cs_count = read_u32(data, &mut pos)? as usize;
    let mut code_strings = Vec::with_capacity(cs_count);
    for _ in 0..cs_count {
        code_strings.push(read_string(data, &mut pos)?);
    }

    Some(VmSnapshot {
        node_id,
        dictionary,
        memory,
        here,
        goals,
        fitness,
        code_strings,
    })
}

// ---------------------------------------------------------------------------
// File system operations
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub fn state_dir(node_id: &NodeId) -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let id_hex = node_id
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();
    format!("{}/.unit/{}", home, id_hex)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save_state(node_id: &NodeId, data: &[u8]) -> Result<(), String> {
    let dir = state_dir(node_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {}", e))?;
    let path = format!("{}/state.bin", dir);
    std::fs::write(&path, data).map_err(|e| format!("write: {}", e))?;
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load_state(node_id: &NodeId) -> Option<Vec<u8>> {
    let path = format!("{}/state.bin", state_dir(node_id));
    std::fs::read(&path).ok()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save_snapshot(node_id: &NodeId, data: &[u8]) -> Result<String, String> {
    let dir = format!("{}/snapshots", state_dir(node_id));
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {}", e))?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let name = format!("{}", ts);
    let path = format!("{}/{}.bin", dir, name);
    std::fs::write(&path, data).map_err(|e| format!("write: {}", e))?;
    Ok(name)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn list_snapshots(node_id: &NodeId) -> Vec<String> {
    let dir = format!("{}/snapshots", state_dir(node_id));
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".bin") {
                names.push(name.trim_end_matches(".bin").to_string());
            }
        }
    }
    names.sort();
    names
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load_snapshot(node_id: &NodeId, name: &str) -> Option<Vec<u8>> {
    let path = format!("{}/snapshots/{}.bin", state_dir(node_id), name);
    std::fs::read(&path).ok()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn delete_state(node_id: &NodeId) -> Result<(), String> {
    let dir = state_dir(node_id);
    if std::fs::metadata(&dir).is_ok() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("rm: {}", e))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Node ID persistence
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
fn node_id_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    format!("{}/.unit/node-id", home)
}

/// Load a previously saved node ID, or return None for first boot.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_node_id() -> Option<NodeId> {
    let path = node_id_path();
    let hex = std::fs::read_to_string(&path).ok()?;
    let hex = hex.trim();
    if hex.len() != 16 {
        return None;
    }
    let mut id = [0u8; 8];
    for i in 0..8 {
        id[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(id)
}

/// Save the node ID so it persists across restarts.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_node_id(id: &NodeId) -> Result<(), String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let dir = format!("{}/.unit", home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {}", e))?;
    let hex: String = id.iter().map(|b| format!("{:02x}", b)).collect();
    std::fs::write(node_id_path(), hex).map_err(|e| format!("write: {}", e))
}

/// Delete the persisted node ID (used by RESET).
#[cfg(not(target_arch = "wasm32"))]
pub fn delete_node_id() -> Result<(), String> {
    let path = node_id_path();
    if std::fs::metadata(&path).is_ok() {
        std::fs::remove_file(&path).map_err(|e| format!("rm: {}", e))?;
    }
    Ok(())
}

/// Rename the state directory from old ID to new ID.
#[cfg(not(target_arch = "wasm32"))]
pub fn rename_state(old_id: &NodeId, new_id: &NodeId) -> Result<(), String> {
    let old_dir = state_dir(old_id);
    let new_dir = state_dir(new_id);
    if std::fs::metadata(&old_dir).is_ok() {
        std::fs::create_dir_all(
            std::path::Path::new(&new_dir)
                .parent()
                .unwrap_or(std::path::Path::new(".")),
        )
        .map_err(|e| format!("mkdir: {}", e))?;
        std::fs::rename(&old_dir, &new_dir).map_err(|e| format!("rename: {}", e))?;
    }
    Ok(())
}

// WASM stubs
#[cfg(target_arch = "wasm32")]
pub fn load_node_id() -> Option<NodeId> {
    None
}
#[cfg(target_arch = "wasm32")]
pub fn save_node_id(_: &NodeId) -> Result<(), String> {
    Ok(())
}
#[cfg(target_arch = "wasm32")]
pub fn delete_node_id() -> Result<(), String> {
    Ok(())
}
#[cfg(target_arch = "wasm32")]
pub fn rename_state(_: &NodeId, _: &NodeId) -> Result<(), String> {
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub fn state_dir(_: &NodeId) -> String {
    String::new()
}
#[cfg(target_arch = "wasm32")]
pub fn save_state(_: &NodeId, _: &[u8]) -> Result<(), String> {
    Err("no persistence on WASM".into())
}
#[cfg(target_arch = "wasm32")]
pub fn load_state(_: &NodeId) -> Option<Vec<u8>> {
    None
}
#[cfg(target_arch = "wasm32")]
pub fn save_snapshot(_: &NodeId, _: &[u8]) -> Result<String, String> {
    Err("no persistence on WASM".into())
}
#[cfg(target_arch = "wasm32")]
pub fn list_snapshots(_: &NodeId) -> Vec<String> {
    vec![]
}
#[cfg(target_arch = "wasm32")]
pub fn load_snapshot(_: &NodeId, _: &str) -> Option<Vec<u8>> {
    None
}
#[cfg(target_arch = "wasm32")]
pub fn delete_state(_: &NodeId) -> Result<(), String> {
    Ok(())
}
