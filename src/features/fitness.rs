// fitness.rs — Fitness evaluation and evolution tracking for unit
//
// Each unit tracks a fitness score derived from task performance.
// The score drives evolution: units below the mesh average adopt
// strategies from fitter peers; units above average experiment with
// mutations.

use crate::mesh::{id_to_hex, NodeId};

// ---------------------------------------------------------------------------
// Fitness tracker
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct FitnessTracker {
    pub score: i64,
    pub tasks_completed: u32,
    pub tasks_failed: u32,
    pub total_time_ms: u64,
    pub benchmark_code: Option<String>,
    pub auto_evolve: bool,
    pub evolution_count: u32,
    pub tasks_since_evolve: u32,
    /// Trigger auto-evolve every N completed tasks.
    pub evolve_interval: u32,
}

impl Default for FitnessTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FitnessTracker {
    pub fn new() -> Self {
        FitnessTracker {
            score: 0,
            tasks_completed: 0,
            tasks_failed: 0,
            total_time_ms: 0,
            benchmark_code: None,
            auto_evolve: false,
            evolution_count: 0,
            tasks_since_evolve: 0,
            evolve_interval: 5,
        }
    }

    /// Record a successful task execution.
    pub fn record_success(&mut self, elapsed_ms: u64) {
        self.tasks_completed += 1;
        self.score += 10;
        // Speed bonus: up to +5 for fast execution.
        let bonus = if elapsed_ms < 100 {
            5
        } else if elapsed_ms < 1000 {
            3
        } else if elapsed_ms < 5000 {
            2
        } else {
            1
        };
        self.score += bonus;
        self.total_time_ms += elapsed_ms;
        self.tasks_since_evolve += 1;
    }

    /// Record a failed task execution.
    pub fn record_failure(&mut self) {
        self.tasks_failed += 1;
        self.score -= 5;
        self.tasks_since_evolve += 1;
    }

    /// Record a peer rating.
    pub fn record_rating(&mut self, rating: i64) {
        self.score += rating;
    }

    /// Check if auto-evolution should trigger.
    pub fn should_auto_evolve(&self) -> bool {
        self.auto_evolve && self.tasks_since_evolve >= self.evolve_interval
    }

    /// Reset the tasks-since-evolve counter after an evolution cycle.
    pub fn mark_evolved(&mut self) {
        self.tasks_since_evolve = 0;
        self.evolution_count += 1;
    }

    /// Format fitness summary for display.
    pub fn format(&self) -> String {
        format!(
            "fitness: {} (completed={}, failed={}, evolutions={})",
            self.score, self.tasks_completed, self.tasks_failed, self.evolution_count
        )
    }

    /// Format a full leaderboard line for this unit.
    pub fn format_line(&self, id: &NodeId) -> String {
        format!(
            "  {} score={} tasks={}/{}",
            id_to_hex(id),
            self.score,
            self.tasks_completed,
            self.tasks_completed + self.tasks_failed
        )
    }
}

// ---------------------------------------------------------------------------
// Leaderboard entry (for peers)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct PeerFitness {
    pub id: NodeId,
    pub score: i64,
}

/// Format a leaderboard from a list of peer fitness entries plus self.
pub fn format_leaderboard(self_id: &NodeId, self_score: i64, peers: &[PeerFitness]) -> String {
    let mut entries: Vec<(NodeId, i64)> = peers.iter().map(|p| (p.id, p.score)).collect();
    entries.push((*self_id, self_score));
    entries.sort_by(|a, b| b.1.cmp(&a.1));

    let mut out = String::from("--- leaderboard ---\n");
    for (i, (id, score)) in entries.iter().enumerate() {
        let marker = if id == self_id { " (you)" } else { "" };
        out.push_str(&format!(
            "  {}. {} score={}{}\n",
            i + 1,
            id_to_hex(id),
            score,
            marker
        ));
    }
    out.push_str("---\n");
    out
}
