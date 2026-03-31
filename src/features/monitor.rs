// monitor.rs — Monitoring, alerting, dashboard, and scheduling for unit
//
// Watches: periodic checks of URLs, files, and processes.
// Alerts: fire when watches detect problems, with Forth handler code.
// Dashboard: formatted overview with sparkline trends.
// Scheduler: recurring Forth commands at intervals.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Watch types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum WatchKind {
    Url(String),
    File(String),
    Process(String),
}

#[derive(Clone, Debug)]
pub struct WatchEntry {
    pub id: u32,
    pub kind: WatchKind,
    pub interval_secs: u64,
    pub last_check: Option<Instant>,
    pub last_status: WatchStatus,
    pub history: Vec<WatchStatus>,
    pub alert_handler: Option<String>, // Forth code to run on alert
    pub alert_level: AlertLevel,
    pub created_at: u64,
}

#[derive(Clone, Debug)]
pub struct WatchStatus {
    pub ok: bool,
    pub code: i32,        // HTTP status or exit code
    pub response_ms: u64, // response time in ms
    pub message: String,  // human-readable status
    pub timestamp: u64,   // unix epoch secs
}

impl WatchStatus {
    pub fn up(code: i32, ms: u64, msg: String) -> Self {
        WatchStatus {
            ok: true,
            code,
            response_ms: ms,
            message: msg,
            timestamp: now_secs(),
        }
    }
    pub fn down(code: i32, msg: String) -> Self {
        WatchStatus {
            ok: false,
            code,
            response_ms: 0,
            message: msg,
            timestamp: now_secs(),
        }
    }
}

// ---------------------------------------------------------------------------
// Alerts
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub enum AlertLevel {
    Info,
    Warn,
    Crit,
}

impl AlertLevel {
    pub fn label(&self) -> &str {
        match self {
            AlertLevel::Info => "INFO",
            AlertLevel::Warn => "WARN",
            AlertLevel::Crit => "CRIT",
        }
    }
    pub fn from_val(v: i64) -> Self {
        match v {
            0 => AlertLevel::Info,
            1 => AlertLevel::Warn,
            _ => AlertLevel::Crit,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Alert {
    pub id: u32,
    pub watch_id: u32,
    pub level: AlertLevel,
    pub message: String,
    pub timestamp: u64,
    pub acknowledged: bool,
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct SchedEntry {
    pub id: u32,
    pub code: String,
    pub interval_secs: u64,
    pub last_run: Option<Instant>,
    pub one_shot: bool,      // true for AT commands
    pub run_at: Option<u64>, // unix epoch for AT
}

// ---------------------------------------------------------------------------
// Monitor state — all monitoring data for one unit
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct MonitorState {
    pub watches: HashMap<u32, WatchEntry>,
    pub alerts: Vec<Alert>,
    pub alert_history: Vec<Alert>,
    pub schedules: HashMap<u32, SchedEntry>,
    next_id: u32,
    pub max_history: usize,
}

impl Default for MonitorState {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitorState {
    pub fn new() -> Self {
        MonitorState {
            watches: HashMap::new(),
            alerts: Vec::new(),
            alert_history: Vec::new(),
            schedules: HashMap::new(),
            next_id: 1,
            max_history: 60, // keep last 60 data points per watch
        }
    }

    fn next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    // -------------------------------------------------------------------
    // Watch management
    // -------------------------------------------------------------------

    pub fn add_watch(&mut self, kind: WatchKind, interval_secs: u64) -> u32 {
        let id = self.next_id();
        let entry = WatchEntry {
            id,
            kind,
            interval_secs,
            last_check: None,
            last_status: WatchStatus {
                ok: true,
                code: 0,
                response_ms: 0,
                message: "pending".into(),
                timestamp: now_secs(),
            },
            history: Vec::new(),
            alert_handler: None,
            alert_level: AlertLevel::Crit,
            created_at: now_secs(),
        };
        self.watches.insert(id, entry);
        id
    }

    pub fn remove_watch(&mut self, id: u32) -> bool {
        self.watches.remove(&id).is_some()
    }

    pub fn set_alert_handler(&mut self, watch_id: u32, code: String) {
        if let Some(w) = self.watches.get_mut(&watch_id) {
            w.alert_handler = Some(code);
        }
    }

    pub fn set_alert_level(&mut self, watch_id: u32, level: AlertLevel) {
        if let Some(w) = self.watches.get_mut(&watch_id) {
            w.alert_level = level;
        }
    }

    /// Record a check result for a watch. Returns an alert if status changed to bad.
    pub fn record_check(&mut self, watch_id: u32, status: WatchStatus) -> Option<Alert> {
        let (was_ok, alert_level) = {
            let w = self.watches.get(&watch_id)?;
            (w.last_status.ok, w.alert_level.clone())
        };
        let w = self.watches.get_mut(&watch_id)?;
        w.last_status = status.clone();
        w.last_check = Some(Instant::now());

        // Keep bounded history.
        w.history.push(status.clone());
        if w.history.len() > self.max_history {
            w.history.remove(0);
        }

        // Fire alert on transition from ok to not-ok.
        if was_ok && !status.ok {
            let alert_id = self.next_id();
            let alert = Alert {
                id: alert_id,
                watch_id,
                level: alert_level,
                message: status.message.clone(),
                timestamp: now_secs(),
                acknowledged: false,
            };
            self.alerts.push(alert.clone());
            return Some(alert);
        }

        // Auto-resolve: if was bad and now ok, remove active alert.
        if !was_ok && status.ok {
            self.alerts.retain(|a| a.watch_id != watch_id);
        }

        None
    }

    pub fn ack_alert(&mut self, alert_id: u32) -> bool {
        for a in &mut self.alerts {
            if a.id == alert_id {
                a.acknowledged = true;
                self.alert_history.push(a.clone());
                // Keep history bounded.
                if self.alert_history.len() > 100 {
                    self.alert_history.remove(0);
                }
                return true;
            }
        }
        false
    }

    /// Get watches that are due for a check.
    pub fn due_watches(&self) -> Vec<u32> {
        self.watches
            .values()
            .filter(|w| match w.last_check {
                None => true,
                Some(t) => t.elapsed() >= Duration::from_secs(w.interval_secs),
            })
            .map(|w| w.id)
            .collect()
    }

    // -------------------------------------------------------------------
    // Scheduler
    // -------------------------------------------------------------------

    pub fn add_schedule(&mut self, code: String, interval_secs: u64) -> u32 {
        let id = self.next_id();
        self.schedules.insert(
            id,
            SchedEntry {
                id,
                code,
                interval_secs,
                last_run: None,
                one_shot: false,
                run_at: None,
            },
        );
        id
    }

    pub fn remove_schedule(&mut self, id: u32) -> bool {
        self.schedules.remove(&id).is_some()
    }

    /// Get scheduled tasks that are due to run. Returns (id, code) pairs.
    pub fn due_schedules(&mut self) -> Vec<(u32, String)> {
        let now_epoch = now_secs();
        let mut due = Vec::new();
        let mut remove = Vec::new();

        for (id, s) in &self.schedules {
            let should_run = match s.last_run {
                None => {
                    if let Some(at) = s.run_at {
                        now_epoch >= at
                    } else {
                        true
                    }
                }
                Some(t) => t.elapsed() >= Duration::from_secs(s.interval_secs),
            };
            if should_run {
                due.push((*id, s.code.clone()));
                if s.one_shot {
                    remove.push(*id);
                }
            }
        }

        // Mark as run.
        for (id, _) in &due {
            if let Some(s) = self.schedules.get_mut(id) {
                s.last_run = Some(Instant::now());
            }
        }

        // Remove one-shots.
        for id in remove {
            self.schedules.remove(&id);
        }

        due
    }

    // -------------------------------------------------------------------
    // Formatting
    // -------------------------------------------------------------------

    pub fn format_watches(&self) -> String {
        if self.watches.is_empty() {
            return "  (no watches)\n".to_string();
        }
        let mut out = String::new();
        let mut watches: Vec<&WatchEntry> = self.watches.values().collect();
        watches.sort_by_key(|w| w.id);
        for w in &watches {
            let kind_str = match &w.kind {
                WatchKind::Url(u) => format!("url:{}", u),
                WatchKind::File(p) => format!("file:{}", p),
                WatchKind::Process(n) => format!("proc:{}", n),
            };
            let status = if w.last_status.ok { "UP" } else { "DOWN" };
            let age = match w.last_check {
                Some(t) => format!("{}s ago", t.elapsed().as_secs()),
                None => "never".into(),
            };
            out.push_str(&format!(
                "  #{} [{}] {} ({}ms) {} checked {}\n",
                w.id, status, kind_str, w.last_status.response_ms, w.last_status.message, age
            ));
        }
        out
    }

    pub fn format_watch_log(&self, watch_id: u32) -> String {
        if let Some(w) = self.watches.get(&watch_id) {
            if w.history.is_empty() {
                return format!("  watch #{}: no history\n", watch_id);
            }
            let mut out = format!(
                "  watch #{} history ({} entries):\n",
                watch_id,
                w.history.len()
            );
            for (i, s) in w.history.iter().enumerate().rev().take(20) {
                out.push_str(&format!(
                    "    {}: {} {}ms {}\n",
                    i,
                    if s.ok { "OK" } else { "FAIL" },
                    s.response_ms,
                    s.message
                ));
            }
            out
        } else {
            format!("  watch #{} not found\n", watch_id)
        }
    }

    pub fn format_alerts(&self) -> String {
        if self.alerts.is_empty() {
            return "  (no active alerts)\n".to_string();
        }
        let mut out = String::new();
        for a in &self.alerts {
            let ack = if a.acknowledged { " [ACK]" } else { "" };
            out.push_str(&format!(
                "  #{} [{}]{} watch #{}: {}\n",
                a.id,
                a.level.label(),
                ack,
                a.watch_id,
                a.message
            ));
        }
        out
    }

    pub fn format_alert_history(&self) -> String {
        if self.alert_history.is_empty() {
            return "  (no alert history)\n".to_string();
        }
        let mut out = String::new();
        for a in self.alert_history.iter().rev().take(20) {
            out.push_str(&format!(
                "  #{} [{}] watch #{}: {}\n",
                a.id,
                a.level.label(),
                a.watch_id,
                a.message
            ));
        }
        out
    }

    pub fn format_schedules(&self) -> String {
        if self.schedules.is_empty() {
            return "  (no scheduled tasks)\n".to_string();
        }
        let mut out = String::new();
        let mut scheds: Vec<&SchedEntry> = self.schedules.values().collect();
        scheds.sort_by_key(|s| s.id);
        for s in &scheds {
            let next = match s.last_run {
                Some(t) => {
                    let remaining = s.interval_secs.saturating_sub(t.elapsed().as_secs());
                    format!("in {}s", remaining)
                }
                None => "now".into(),
            };
            out.push_str(&format!(
                "  #{} every {}s next={}: {}\n",
                s.id,
                s.interval_secs,
                next,
                s.code.chars().take(40).collect::<String>()
            ));
        }
        out
    }

    pub fn format_dashboard(&self, peer_count: usize, fitness: i64, goal_summary: &str) -> String {
        let mut out = String::from("╔══════════════════════════════════════╗\n");
        out.push_str("║         UNIT OPS DASHBOARD           ║\n");
        out.push_str("╚══════════════════════════════════════╝\n");

        // Watches.
        out.push_str("─── watches ───\n");
        if self.watches.is_empty() {
            out.push_str("  (none)\n");
        } else {
            let mut watches: Vec<&WatchEntry> = self.watches.values().collect();
            watches.sort_by_key(|w| w.id);
            for w in &watches {
                let name = match &w.kind {
                    WatchKind::Url(u) => u.clone(),
                    WatchKind::File(p) => p.clone(),
                    WatchKind::Process(n) => n.clone(),
                };
                let status = if w.last_status.ok { "UP  " } else { "DOWN" };
                let spark = sparkline(&w.history);
                out.push_str(&format!(
                    "  #{} [{}] {} {} {}\n",
                    w.id,
                    status,
                    spark,
                    w.last_status.response_ms,
                    name.chars().take(30).collect::<String>()
                ));
            }
        }

        // Alerts.
        out.push_str("─── alerts ───\n");
        let active = self.alerts.iter().filter(|a| !a.acknowledged).count();
        if active == 0 {
            out.push_str("  all clear\n");
        } else {
            for a in self.alerts.iter().filter(|a| !a.acknowledged) {
                out.push_str(&format!(
                    "  [{}] watch #{}: {}\n",
                    a.level.label(),
                    a.watch_id,
                    a.message
                ));
            }
        }

        // Mesh.
        out.push_str("─── mesh ───\n");
        out.push_str(&format!("  peers: {}  fitness: {}\n", peer_count, fitness));

        // Goals.
        if !goal_summary.is_empty() {
            out.push_str("─── goals ───\n");
            out.push_str(goal_summary);
        }

        out
    }

    /// Compute overall health score (0-100).
    pub fn health_score(&self, peer_count: usize, fitness: i64) -> i64 {
        let total_watches = self.watches.len() as i64;
        let healthy = self.watches.values().filter(|w| w.last_status.ok).count() as i64;
        let watch_score = if total_watches > 0 {
            (healthy * 100) / total_watches
        } else {
            100
        };
        let active_alerts = self.alerts.iter().filter(|a| !a.acknowledged).count() as i64;
        let alert_penalty = (active_alerts * 20).min(50);
        let peer_bonus = (peer_count as i64 * 5).min(20);
        let fitness_bonus = (fitness / 10).clamp(0, 10);
        (watch_score - alert_penalty + peer_bonus + fitness_bonus).clamp(0, 100)
    }

    /// Compute uptime percentage for a watch.
    pub fn uptime(&self, watch_id: u32) -> f64 {
        if let Some(w) = self.watches.get(&watch_id) {
            if w.history.is_empty() {
                return 100.0;
            }
            let ok = w.history.iter().filter(|s| s.ok).count();
            (ok as f64 / w.history.len() as f64) * 100.0
        } else {
            0.0
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Generate an ASCII sparkline from watch history response times.
fn sparkline(history: &[WatchStatus]) -> String {
    if history.is_empty() {
        return String::from("        ");
    }
    let bars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let times: Vec<u64> = history
        .iter()
        .rev()
        .take(8)
        .rev()
        .map(|s| s.response_ms)
        .collect();
    let max = *times.iter().max().unwrap_or(&1);
    let max = max.max(1);
    times
        .iter()
        .map(|&t| {
            let idx = ((t * 7) / max) as usize;
            bars[idx.min(7)]
        })
        .collect()
}
