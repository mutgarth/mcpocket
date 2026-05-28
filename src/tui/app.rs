use std::collections::VecDeque;

use crate::doctor::DoctorCheck;
use crate::router::ToolInspectServer;
use crate::telemetry::{CallStatus, Event};
use crate::upstream::StatusRow;

/// Max tool-call events retained for the Live feed.
pub const MAX_LIVE_EVENTS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Servers,
    Tools,
    Live,
    Doctor,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Servers, Tab::Tools, Tab::Live, Tab::Doctor];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Servers => "Servers",
            Tab::Tools => "Tools",
            Tab::Live => "Live",
            Tab::Doctor => "Doctor",
        }
    }
}

/// A flattened view of a tool-call event for the Live feed.
#[derive(Debug, Clone)]
pub struct LiveEvent {
    pub ts: u64,
    pub client: String,
    pub tool: String,
    pub duration_ms: u64,
    pub status: CallStatus,
}

pub struct App {
    pub tab: Tab,
    pub selected: usize,
    pub servers: Vec<StatusRow>,
    pub tools: Vec<ToolInspectServer>,
    pub doctor: Vec<DoctorCheck>,
    pub live_events: VecDeque<LiveEvent>,
    pub status_message: Option<String>,
    pub should_quit: bool,
    pub dirty: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            tab: Tab::Servers,
            selected: 0,
            servers: Vec::new(),
            tools: Vec::new(),
            doctor: Vec::new(),
            live_events: VecDeque::with_capacity(MAX_LIVE_EVENTS),
            status_message: None,
            should_quit: false,
            dirty: true,
        }
    }

    pub fn next_tab(&mut self) {
        let idx = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = Tab::ALL[(idx + 1) % Tab::ALL.len()];
        self.dirty = true;
    }

    pub fn prev_tab(&mut self) {
        let idx = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = Tab::ALL[(idx + Tab::ALL.len() - 1) % Tab::ALL.len()];
        self.dirty = true;
    }

    /// Fold a telemetry event into the live feed. `Hello` frames are ignored.
    pub fn ingest(&mut self, event: Event) {
        if let Event::ToolCall {
            ts,
            client,
            tool,
            duration_ms,
            status,
            ..
        } = event
        {
            if self.live_events.len() == MAX_LIVE_EVENTS {
                self.live_events.pop_front();
            }
            self.live_events.push_back(LiveEvent {
                ts,
                client,
                tool,
                duration_ms,
                status,
            });
            self.dirty = true;
        }
    }

    pub fn error_count(&self) -> usize {
        self.live_events
            .iter()
            .filter(|e| e.status == CallStatus::Error)
            .count()
    }

    /// 95th-percentile latency over the retained feed (nearest-rank).
    pub fn p95_latency(&self) -> Option<u64> {
        if self.live_events.is_empty() {
            return None;
        }
        let mut durs: Vec<u64> = self.live_events.iter().map(|e| e.duration_ms).collect();
        durs.sort_unstable();
        let rank = ((durs.len() as f64) * 0.95).ceil() as usize;
        let idx = rank.saturating_sub(1).min(durs.len() - 1);
        Some(durs[idx])
    }

    /// Requests per second over the `window_ms` window ending at `now_ms`.
    pub fn req_per_sec(&self, now_ms: u64, window_ms: u64) -> f64 {
        let start = now_ms.saturating_sub(window_ms);
        let count = self
            .live_events
            .iter()
            .filter(|e| e.ts >= start && e.ts < now_ms)
            .count();
        count as f64 / (window_ms as f64 / 1000.0)
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{CallStatus, Event};

    fn call(ts: u64, dur: u64, status: CallStatus) -> Event {
        Event::ToolCall {
            ts,
            pid: 1,
            client: "c".to_owned(),
            server: "github".to_owned(),
            tool: "github__x".to_owned(),
            duration_ms: dur,
            status,
        }
    }

    #[test]
    fn tab_cycles_forward_and_back() {
        let mut app = App::new();
        assert_eq!(app.tab, Tab::Servers);
        app.next_tab();
        assert_eq!(app.tab, Tab::Tools);
        app.prev_tab();
        assert_eq!(app.tab, Tab::Servers);
        app.prev_tab();
        assert_eq!(app.tab, Tab::Doctor); // wraps
    }

    #[test]
    fn hello_event_is_not_counted_as_traffic() {
        let mut app = App::new();
        app.ingest(Event::Hello {
            pid: 1,
            client: "c".to_owned(),
            version: "0".to_owned(),
        });
        assert_eq!(app.live_events.len(), 0);
    }

    #[test]
    fn history_is_bounded() {
        let mut app = App::new();
        for i in 0..(MAX_LIVE_EVENTS + 50) {
            app.ingest(call(i as u64, 1, CallStatus::Ok));
        }
        assert_eq!(app.live_events.len(), MAX_LIVE_EVENTS);
    }

    #[test]
    fn error_count_and_p95() {
        let mut app = App::new();
        app.ingest(call(1, 10, CallStatus::Ok));
        app.ingest(call(2, 20, CallStatus::Error));
        app.ingest(call(3, 30, CallStatus::Ok));
        assert_eq!(app.error_count(), 1);
        assert_eq!(app.p95_latency(), Some(30));
    }

    #[test]
    fn req_per_sec_counts_within_window() {
        let mut app = App::new();
        app.ingest(call(1_000, 1, CallStatus::Ok));
        app.ingest(call(1_500, 1, CallStatus::Ok));
        app.ingest(call(50_000, 1, CallStatus::Ok)); // outside 10s window from now=51_000
        // 2 events within [41_000, 51_000)? Only ts=50_000 qualifies -> recheck:
        // window is last 10s ending at now.
        assert_eq!(app.req_per_sec(51_000, 10_000), 0.1); // 1 event / 10s
    }
}
