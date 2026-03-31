// goals.rs — Goal and task management for unit mesh
//
// Goals are human-provided high-level objectives. Tasks are unit-level
// work items derived from goals. The GoalRegistry is shared across the
// mesh via gossip so all units maintain a consistent view of current work.
//
// Goals can carry Forth code payloads. When a unit claims such a task,
// it compiles and executes the Forth code in a sandbox, captures the
// resulting stack and output, and propagates the result through the mesh.
//
// Humans set direction, the mesh navigates.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::mesh::{id_to_hex, NodeId};
use crate::types::Cell;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

pub type GoalId = u64;
pub type TaskId = u64;

#[derive(Clone, Debug, PartialEq)]
pub enum GoalStatus {
    Pending,   // submitted, no tasks claimed yet
    Active,    // at least one task is being worked
    Completed, // all tasks done
    Failed,    // cancelled or failed
}

impl GoalStatus {
    pub fn as_u8(&self) -> u8 {
        match self {
            GoalStatus::Pending => 0,
            GoalStatus::Active => 1,
            GoalStatus::Completed => 2,
            GoalStatus::Failed => 3,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => GoalStatus::Active,
            2 => GoalStatus::Completed,
            3 => GoalStatus::Failed,
            _ => GoalStatus::Pending,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            GoalStatus::Pending => "pending",
            GoalStatus::Active => "active",
            GoalStatus::Completed => "completed",
            GoalStatus::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TaskStatus {
    Waiting, // unclaimed, in queue
    Running, // claimed by a unit
    Done,    // completed successfully
    Failed,  // failed or cancelled
}

impl TaskStatus {
    pub fn as_u8(&self) -> u8 {
        match self {
            TaskStatus::Waiting => 0,
            TaskStatus::Running => 1,
            TaskStatus::Done => 2,
            TaskStatus::Failed => 3,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => TaskStatus::Running,
            2 => TaskStatus::Done,
            3 => TaskStatus::Failed,
            _ => TaskStatus::Waiting,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            TaskStatus::Waiting => "waiting",
            TaskStatus::Running => "running",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
        }
    }
}

// ---------------------------------------------------------------------------
// TaskResult — captured output from executing a Forth code payload
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct TaskResult {
    /// Stack contents after execution.
    pub stack_snapshot: Vec<Cell>,
    /// Captured printed output (from ., .S, EMIT, .", TYPE, etc.).
    pub output: String,
    /// Whether execution completed without error.
    pub success: bool,
    /// Error message if execution failed (timeout, stack underflow, etc.).
    pub error: Option<String>,
}

impl TaskResult {
    pub fn format(&self) -> String {
        let mut out = String::new();
        if self.success {
            out.push_str("  status: ok\n");
        } else {
            out.push_str(&format!(
                "  status: FAILED — {}\n",
                self.error.as_deref().unwrap_or("unknown error")
            ));
        }
        if !self.stack_snapshot.is_empty() {
            out.push_str("  stack: ");
            for val in &self.stack_snapshot {
                out.push_str(&format!("{} ", val));
            }
            out.push('\n');
        }
        if !self.output.is_empty() {
            out.push_str(&format!("  output: {}\n", self.output.trim_end()));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Goal
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Goal {
    pub id: GoalId,
    pub description: String,
    /// Forth code payload — if Some, this goal is executable.
    pub code: Option<String>,
    pub priority: Cell,
    pub status: GoalStatus,
    pub created_at: u64,
    pub creator: NodeId,
    pub task_ids: Vec<TaskId>,
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Task {
    pub id: TaskId,
    pub goal_id: GoalId,
    pub description: String,
    /// Per-task code (for SPLIT subtasks; None = use parent goal's code).
    pub code: Option<String>,
    pub assigned_to: Option<NodeId>,
    pub status: TaskStatus,
    pub result: Option<TaskResult>,
    pub created_at: u64,
}

// ---------------------------------------------------------------------------
// GoalRegistry — manages all goals and tasks for a unit
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct GoalRegistry {
    pub goals: HashMap<GoalId, Goal>,
    pub tasks: HashMap<TaskId, Task>,
    id_counter: u64,
}

impl GoalRegistry {
    /// Create a new registry. The counter is seeded from the node ID to
    /// avoid collisions between nodes generating IDs concurrently.
    pub fn new(node_id: &NodeId) -> Self {
        let base = ((node_id[6] as u64) << 4 | (node_id[7] as u64 >> 4)) * 10 + 1;
        GoalRegistry {
            goals: HashMap::new(),
            tasks: HashMap::new(),
            id_counter: base,
        }
    }

    /// Create an empty registry (for deserialization).
    pub fn empty() -> Self {
        GoalRegistry {
            goals: HashMap::new(),
            tasks: HashMap::new(),
            id_counter: 1,
        }
    }

    fn next_id(&mut self) -> u64 {
        let id = self.id_counter;
        self.id_counter += 1;
        id
    }

    fn now_millis() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    // -------------------------------------------------------------------
    // Goal lifecycle
    // -------------------------------------------------------------------

    /// Create a new goal with one initial task. Returns the goal ID.
    /// If `code` is Some, the goal carries executable Forth code.
    pub fn create_goal(
        &mut self,
        description: String,
        priority: Cell,
        creator: NodeId,
        code: Option<String>,
    ) -> GoalId {
        let goal_id = self.next_id();
        let task_id = self.next_id();
        let now = Self::now_millis();

        let task = Task {
            id: task_id,
            goal_id,
            description: description.clone(),
            code: None,
            assigned_to: None,
            status: TaskStatus::Waiting,
            result: None,
            created_at: now,
        };

        let goal = Goal {
            id: goal_id,
            description,
            code,
            priority,
            status: GoalStatus::Pending,
            created_at: now,
            creator,
            task_ids: vec![task_id],
        };

        self.tasks.insert(task_id, task);
        self.goals.insert(goal_id, goal);
        goal_id
    }

    /// Claim the highest-priority unclaimed task.
    /// Returns (task_id, goal_id, description) or None if nothing available.
    pub fn claim_task(&mut self, node_id: NodeId) -> Option<(TaskId, GoalId, String)> {
        let mut candidates: Vec<(TaskId, Cell)> = self
            .tasks
            .iter()
            .filter(|(_, t)| t.status == TaskStatus::Waiting)
            .filter_map(|(tid, t)| self.goals.get(&t.goal_id).map(|g| (*tid, g.priority)))
            .collect();

        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        if let Some(&(task_id, _)) = candidates.first() {
            if let Some(task) = self.tasks.get_mut(&task_id) {
                task.assigned_to = Some(node_id);
                task.status = TaskStatus::Running;
                let goal_id = task.goal_id;
                let desc = task.description.clone();

                if let Some(goal) = self.goals.get_mut(&goal_id) {
                    if goal.status == GoalStatus::Pending {
                        goal.status = GoalStatus::Active;
                    }
                }
                return Some((task_id, goal_id, desc));
            }
        }
        None
    }

    /// Claim the highest-priority unclaimed *executable* task.
    /// Returns (task_id, goal_id, description, code).
    pub fn claim_executable_task(
        &mut self,
        node_id: NodeId,
    ) -> Option<(TaskId, GoalId, String, String)> {
        // Find tasks that have executable code (per-task or via parent goal).
        let mut candidates: Vec<(TaskId, Cell)> = self
            .tasks
            .iter()
            .filter(|(_, t)| t.status == TaskStatus::Waiting)
            .filter_map(|(tid, t)| {
                // Task has code itself, or parent goal has code.
                let has_code = t.code.is_some()
                    || self
                        .goals
                        .get(&t.goal_id)
                        .and_then(|g| g.code.as_ref())
                        .is_some();
                if has_code {
                    self.goals.get(&t.goal_id).map(|g| (*tid, g.priority))
                } else {
                    None
                }
            })
            .collect();

        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        if let Some(&(task_id, _)) = candidates.first() {
            if let Some(task) = self.tasks.get_mut(&task_id) {
                task.assigned_to = Some(node_id);
                task.status = TaskStatus::Running;
                let goal_id = task.goal_id;
                let desc = task.description.clone();
                // Use per-task code (SPLIT subtasks) or fall back to goal code.
                let code = task
                    .code
                    .clone()
                    .or_else(|| self.goals.get(&goal_id).and_then(|g| g.code.clone()))
                    .unwrap_or_default();

                if let Some(goal) = self.goals.get_mut(&goal_id) {
                    if goal.status == GoalStatus::Pending {
                        goal.status = GoalStatus::Active;
                    }
                }
                return Some((task_id, goal_id, desc, code));
            }
        }
        None
    }

    /// Mark a task as done with a full result. Completes the parent goal
    /// if all tasks are done.
    pub fn complete_task(&mut self, task_id: TaskId, result: Option<TaskResult>) -> bool {
        let goal_id = if let Some(task) = self.tasks.get_mut(&task_id) {
            let success = result.as_ref().map(|r| r.success).unwrap_or(true);
            task.status = if success {
                TaskStatus::Done
            } else {
                TaskStatus::Failed
            };
            task.result = result;
            task.goal_id
        } else {
            return false;
        };

        // Check if all tasks for this goal are now done.
        if let Some(goal) = self.goals.get(&goal_id) {
            let all_done = goal.task_ids.iter().all(|tid| {
                self.tasks
                    .get(tid)
                    .map(|t| t.status == TaskStatus::Done || t.status == TaskStatus::Failed)
                    .unwrap_or(true)
            });
            if all_done {
                let all_success = goal.task_ids.iter().all(|tid| {
                    self.tasks
                        .get(tid)
                        .map(|t| t.status == TaskStatus::Done)
                        .unwrap_or(true)
                });
                if let Some(g) = self.goals.get_mut(&goal_id) {
                    g.status = if all_success {
                        GoalStatus::Completed
                    } else {
                        GoalStatus::Failed
                    };
                }
            }
        }
        true
    }

    /// Cancel a goal and all its non-completed tasks.
    pub fn cancel_goal(&mut self, goal_id: GoalId) -> bool {
        if let Some(goal) = self.goals.get_mut(&goal_id) {
            goal.status = GoalStatus::Failed;
            let task_ids = goal.task_ids.clone();
            for tid in &task_ids {
                if let Some(task) = self.tasks.get_mut(tid) {
                    if task.status != TaskStatus::Done {
                        task.status = TaskStatus::Failed;
                    }
                }
            }
            true
        } else {
            false
        }
    }

    /// Change a goal's priority.
    pub fn steer_goal(&mut self, goal_id: GoalId, new_priority: Cell) -> bool {
        if let Some(goal) = self.goals.get_mut(&goal_id) {
            goal.priority = new_priority;
            true
        } else {
            false
        }
    }

    // -------------------------------------------------------------------
    // Gossip convergence
    // -------------------------------------------------------------------

    pub fn merge_goal(&mut self, goal: Goal) {
        if let Some(existing) = self.goals.get(&goal.id) {
            if goal.status.as_u8() > existing.status.as_u8() || goal.priority != existing.priority {
                let mut merged_tasks = existing.task_ids.clone();
                for tid in &goal.task_ids {
                    if !merged_tasks.contains(tid) {
                        merged_tasks.push(*tid);
                    }
                }
                let mut merged = goal;
                merged.task_ids = merged_tasks;
                self.goals.insert(merged.id, merged);
            }
        } else {
            if goal.id >= self.id_counter {
                self.id_counter = goal.id + 1;
            }
            self.goals.insert(goal.id, goal);
        }
    }

    pub fn merge_task(&mut self, task: Task) {
        if let Some(existing) = self.tasks.get(&task.id) {
            if task.status.as_u8() > existing.status.as_u8() {
                self.tasks.insert(task.id, task);
            }
        } else {
            if task.id >= self.id_counter {
                self.id_counter = task.id + 1;
            }
            self.tasks.insert(task.id, task);
        }
    }

    // -------------------------------------------------------------------
    // Queries
    // -------------------------------------------------------------------

    pub fn pending_task_count(&self) -> usize {
        self.tasks
            .values()
            .filter(|t| t.status == TaskStatus::Waiting)
            .count()
    }

    pub fn active_goal_count(&self) -> usize {
        self.goals
            .values()
            .filter(|g| g.status == GoalStatus::Pending || g.status == GoalStatus::Active)
            .count()
    }

    /// Get the code payload for a goal, if it's executable.
    pub fn goal_code(&self, goal_id: GoalId) -> Option<String> {
        self.goals.get(&goal_id).and_then(|g| g.code.clone())
    }

    // -------------------------------------------------------------------
    // Task decomposition
    // -------------------------------------------------------------------

    /// Add a subtask to an existing goal. Returns the task ID.
    pub fn create_subtask(
        &mut self,
        goal_id: GoalId,
        description: String,
        code: Option<String>,
    ) -> Option<TaskId> {
        if !self.goals.contains_key(&goal_id) {
            return None;
        }
        let task_id = self.next_id();
        let now = Self::now_millis();
        let task = Task {
            id: task_id,
            goal_id,
            description: description.clone(),
            code: code.clone(),
            assigned_to: None,
            status: TaskStatus::Waiting,
            result: None,
            created_at: now,
        };
        self.tasks.insert(task_id, task);
        if let Some(goal) = self.goals.get_mut(&goal_id) {
            goal.task_ids.push(task_id);
            if code.is_some() && goal.code.is_none() {
                goal.code = code;
            }
        }
        Some(task_id)
    }

    /// Create a goal with N subtasks from a SPLIT directive.
    /// `total` is the iteration count, `n` is the split count,
    /// `remaining_code` is the Forth code after SPLIT.
    pub fn create_split_goal(
        &mut self,
        total: Cell,
        n: Cell,
        remaining_code: &str,
        priority: Cell,
        creator: NodeId,
    ) -> GoalId {
        let n = n.max(1) as usize;
        let chunk = total / n as Cell;
        let goal_id = self.next_id();
        let now = Self::now_millis();

        let description = format!(
            "{}×{}: {}",
            n,
            chunk,
            remaining_code.chars().take(40).collect::<String>()
        );

        let mut task_ids = Vec::with_capacity(n);
        for k in 0..n {
            let start = k as Cell * chunk;
            let end = if k == n - 1 { total } else { start + chunk };
            let task_code = format!("{} {} {}", start, end, remaining_code);
            let task_id = self.next_id();
            let task = Task {
                id: task_id,
                goal_id,
                description: format!(
                    "chunk {}/{}: {}",
                    k + 1,
                    n,
                    task_code.chars().take(30).collect::<String>()
                ),
                code: Some(task_code),
                assigned_to: None,
                status: TaskStatus::Waiting,
                result: None,
                created_at: now,
            };
            self.tasks.insert(task_id, task);
            task_ids.push(task_id);
        }

        let full_code = format!("{} {} SPLIT {}", total, n, remaining_code);
        let goal = Goal {
            id: goal_id,
            description,
            code: Some(full_code),
            priority,
            status: GoalStatus::Pending,
            created_at: now,
            creator,
            task_ids,
        };
        self.goals.insert(goal_id, goal);
        goal_id
    }

    /// Fork an existing single-task goal into N tasks.
    pub fn fork_goal(&mut self, goal_id: GoalId, n: usize) -> bool {
        let code = match self.goals.get(&goal_id) {
            Some(g) => match &g.code {
                Some(c) => c.clone(),
                None => return false,
            },
            None => return false,
        };

        let now = Self::now_millis();
        // Create N-1 additional tasks (goal already has 1).
        for k in 1..n {
            let task_id = self.next_id();
            let task = Task {
                id: task_id,
                goal_id,
                description: format!(
                    "fork {}/{}: {}",
                    k + 1,
                    n,
                    code.chars().take(30).collect::<String>()
                ),
                code: None,
                assigned_to: None,
                status: TaskStatus::Waiting,
                result: None,
                created_at: now,
            };
            self.tasks.insert(task_id, task);
            if let Some(goal) = self.goals.get_mut(&goal_id) {
                goal.task_ids.push(task_id);
            }
        }
        true
    }

    /// Format progress for a goal: "3/10 subtasks completed"
    pub fn format_progress(&self, goal_id: GoalId) -> String {
        if let Some(goal) = self.goals.get(&goal_id) {
            let total = goal.task_ids.len();
            let done = goal
                .task_ids
                .iter()
                .filter(|tid| {
                    self.tasks
                        .get(tid)
                        .map(|t| t.status == TaskStatus::Done)
                        .unwrap_or(false)
                })
                .count();
            let failed = goal
                .task_ids
                .iter()
                .filter(|tid| {
                    self.tasks
                        .get(tid)
                        .map(|t| t.status == TaskStatus::Failed)
                        .unwrap_or(false)
                })
                .count();
            let running = goal
                .task_ids
                .iter()
                .filter(|tid| {
                    self.tasks
                        .get(tid)
                        .map(|t| t.status == TaskStatus::Running)
                        .unwrap_or(false)
                })
                .count();
            format!(
                "goal #{} [{}]: {}/{} done, {} running, {} failed\n",
                goal.id,
                goal.status.label(),
                done,
                total,
                running,
                failed
            )
        } else {
            format!("goal #{} not found\n", goal_id)
        }
    }

    /// Collect all subtask results for a goal as a flat list.
    pub fn collect_results(&self, goal_id: GoalId) -> Vec<(TaskId, Option<&TaskResult>)> {
        if let Some(goal) = self.goals.get(&goal_id) {
            goal.task_ids
                .iter()
                .filter_map(|tid| self.tasks.get(tid).map(|t| (*tid, t.result.as_ref())))
                .collect()
        } else {
            vec![]
        }
    }

    /// Get the executable code for a specific task.
    /// Per-task code (from SPLIT) takes priority; falls back to goal code.
    pub fn task_code(&self, task_id: TaskId) -> Option<String> {
        let task = self.tasks.get(&task_id)?;
        if let Some(ref code) = task.code {
            return Some(code.clone());
        }
        self.goals.get(&task.goal_id).and_then(|g| g.code.clone())
    }

    // -------------------------------------------------------------------
    // Formatting
    // -------------------------------------------------------------------

    pub fn format_goals(&self) -> String {
        if self.goals.is_empty() {
            return "  (no goals)\n".to_string();
        }
        let mut goals: Vec<&Goal> = self.goals.values().collect();
        goals.sort_by(|a, b| b.priority.cmp(&a.priority));

        let mut out = String::new();
        for g in &goals {
            let total = g.task_ids.len();
            let done = g
                .task_ids
                .iter()
                .filter(|tid| {
                    self.tasks
                        .get(tid)
                        .map(|t| t.status == TaskStatus::Done)
                        .unwrap_or(false)
                })
                .count();
            let exec_marker = if g.code.is_some() { " [exec]" } else { "" };
            out.push_str(&format!(
                "  #{} [{}]{} p={} ({}/{} tasks): {}\n",
                g.id,
                g.status.label(),
                exec_marker,
                g.priority,
                done,
                total,
                g.description
            ));
        }
        out
    }

    pub fn format_my_tasks(&self, node_id: &NodeId) -> String {
        let mut my_tasks: Vec<&Task> = self
            .tasks
            .values()
            .filter(|t| t.assigned_to.as_ref() == Some(node_id))
            .collect();

        if my_tasks.is_empty() {
            return "  (no tasks claimed)\n".to_string();
        }

        my_tasks.sort_by_key(|t| std::cmp::Reverse(t.created_at));

        let mut out = String::new();
        for t in &my_tasks {
            out.push_str(&format!(
                "  task #{} [{}] goal #{}: {}\n",
                t.id,
                t.status.label(),
                t.goal_id,
                t.description
            ));
            if let Some(ref result) = t.result {
                out.push_str(&result.format());
            }
        }
        out
    }

    pub fn format_goal_tasks(&self, goal_id: GoalId) -> String {
        if let Some(goal) = self.goals.get(&goal_id) {
            let mut out = format!(
                "goal #{} [{}] p={}: {}\n",
                goal.id,
                goal.status.label(),
                goal.priority,
                goal.description
            );
            if let Some(ref code) = goal.code {
                out.push_str(&format!("  code: {}\n", code));
            }
            out.push_str(&format!("  creator: {}\n", id_to_hex(&goal.creator)));
            for tid in &goal.task_ids {
                if let Some(task) = self.tasks.get(tid) {
                    let assignee = task
                        .assigned_to
                        .as_ref()
                        .map(id_to_hex)
                        .unwrap_or_else(|| "unassigned".to_string());
                    out.push_str(&format!(
                        "  task #{} [{}] -> {}\n",
                        task.id,
                        task.status.label(),
                        assignee,
                    ));
                    if let Some(ref result) = task.result {
                        out.push_str(&result.format());
                    }
                }
            }
            out
        } else {
            format!("goal #{} not found\n", goal_id)
        }
    }

    /// Format result for a specific task.
    pub fn format_task_result(&self, task_id: TaskId) -> String {
        if let Some(task) = self.tasks.get(&task_id) {
            let mut out = format!(
                "task #{} [{}] goal #{}:\n",
                task.id,
                task.status.label(),
                task.goal_id,
            );
            if let Some(ref result) = task.result {
                out.push_str(&result.format());
            } else {
                out.push_str("  (no result yet)\n");
            }
            out
        } else {
            format!("task #{} not found\n", task_id)
        }
    }

    /// Format combined results for all tasks of a goal.
    pub fn format_goal_result(&self, goal_id: GoalId) -> String {
        if let Some(goal) = self.goals.get(&goal_id) {
            let mut out = format!(
                "goal #{} [{}]: {}\n",
                goal.id,
                goal.status.label(),
                goal.description,
            );
            for tid in &goal.task_ids {
                if let Some(task) = self.tasks.get(tid) {
                    out.push_str(&format!("  task #{}:\n", task.id));
                    if let Some(ref result) = task.result {
                        out.push_str(&result.format());
                    } else {
                        out.push_str("    (pending)\n");
                    }
                }
            }
            out
        } else {
            format!("goal #{} not found\n", goal_id)
        }
    }

    pub fn format_report(&self) -> String {
        let total_goals = self.goals.len();
        let g_pending = self
            .goals
            .values()
            .filter(|g| g.status == GoalStatus::Pending)
            .count();
        let g_active = self
            .goals
            .values()
            .filter(|g| g.status == GoalStatus::Active)
            .count();
        let g_done = self
            .goals
            .values()
            .filter(|g| g.status == GoalStatus::Completed)
            .count();
        let g_failed = self
            .goals
            .values()
            .filter(|g| g.status == GoalStatus::Failed)
            .count();

        let total_tasks = self.tasks.len();
        let t_waiting = self
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Waiting)
            .count();
        let t_running = self
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Running)
            .count();
        let t_done = self
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Done)
            .count();
        let t_failed = self
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Failed)
            .count();

        let mut workers: Vec<NodeId> = self
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Running)
            .filter_map(|t| t.assigned_to)
            .collect();
        workers.sort();
        workers.dedup();

        let exec_goals = self.goals.values().filter(|g| g.code.is_some()).count();

        let mut out = String::from("--- mesh progress report ---\n");
        out.push_str(&format!(
            "goals: {} total ({} pending, {} active, {} completed, {} failed, {} executable)\n",
            total_goals, g_pending, g_active, g_done, g_failed, exec_goals
        ));
        out.push_str(&format!(
            "tasks: {} total ({} waiting, {} running, {} done, {} failed)\n",
            total_tasks, t_waiting, t_running, t_done, t_failed
        ));
        out.push_str(&format!("workers: {} active units\n", workers.len()));
        out.push_str("---\n");
        out
    }
}
