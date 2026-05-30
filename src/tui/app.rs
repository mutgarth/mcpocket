use std::collections::{BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use crate::config_edit::ServerProfileListRow;
use crate::doctor::DoctorCheck;
use crate::router::ToolInspectServer;
use crate::telemetry::{CallStatus, Event};
use crate::upstream::StatusRow;

/// Max tool-call events retained for the Live feed.
pub const MAX_LIVE_EVENTS: usize = 500;
pub const STATUS_MESSAGE_TTL: Duration = Duration::from_secs(2);

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextInputMode {
    NewServerProfile {
        server: String,
    },
    EditServerParameter {
        server: String,
        profile: Option<String>,
        field: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextInput {
    pub mode: TextInputMode,
    pub prompt: String,
    pub value: String,
}

pub struct App {
    pub tab: Tab,
    pub selected: usize,
    pub server_profile_server: Option<String>,
    pub server_profile_return_selected: usize,
    pub servers: Vec<StatusRow>,
    pub server_profiles: Vec<ServerProfileListRow>,
    pub tools: Vec<ToolInspectServer>,
    pub tools_expanded: BTreeSet<String>,
    pub doctor: Vec<DoctorCheck>,
    pub live_events: VecDeque<LiveEvent>,
    pub status_message: Option<String>,
    pub text_input: Option<TextInput>,
    pub refreshing: bool,
    status_message_until: Option<Instant>,
    pub should_quit: bool,
    pub dirty: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            tab: Tab::Servers,
            selected: 0,
            server_profile_server: None,
            server_profile_return_selected: 0,
            servers: Vec::new(),
            server_profiles: Vec::new(),
            tools: Vec::new(),
            tools_expanded: BTreeSet::new(),
            doctor: Vec::new(),
            live_events: VecDeque::with_capacity(MAX_LIVE_EVENTS),
            status_message: None,
            text_input: None,
            refreshing: false,
            status_message_until: None,
            should_quit: false,
            dirty: true,
        }
    }

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status_message = Some(message.into());
        self.status_message_until = Some(Instant::now() + STATUS_MESSAGE_TTL);
        self.dirty = true;
    }

    pub fn clear_expired_status(&mut self, now: Instant) {
        if self.status_message.is_some()
            && self
                .status_message_until
                .is_some_and(|deadline| now >= deadline)
        {
            self.status_message = None;
            self.status_message_until = None;
            self.dirty = true;
        }
    }

    pub fn is_tools_expanded(&self, server: &str) -> bool {
        self.tools_expanded.contains(server)
    }

    pub fn toggle_tools_expanded(&mut self, server: &str) {
        if !self.tools_expanded.remove(server) {
            self.tools_expanded.insert(server.to_owned());
        }
        self.dirty = true;
    }

    pub fn is_server_profile_open(&self) -> bool {
        self.server_profile_server.is_some()
    }

    pub fn open_server_profile(&mut self, server: String, selected_profile: usize) {
        self.server_profile_server = Some(server);
        self.server_profile_return_selected = self.selected;
        self.selected = selected_profile;
        self.dirty = true;
    }

    pub fn close_server_profile(&mut self) {
        self.server_profile_server = None;
        self.selected = self.server_profile_return_selected;
        self.dirty = true;
    }

    pub fn open_text_input(
        &mut self,
        mode: TextInputMode,
        prompt: impl Into<String>,
        value: impl Into<String>,
    ) {
        self.text_input = Some(TextInput {
            mode,
            prompt: prompt.into(),
            value: value.into(),
        });
        self.dirty = true;
    }

    pub fn close_text_input(&mut self) {
        self.text_input = None;
        self.dirty = true;
    }

    pub fn is_text_input_open(&self) -> bool {
        self.text_input.is_some()
    }

    pub fn text_input_prefix_len(&self) -> usize {
        let Some(input) = &self.text_input else {
            return 0;
        };
        match &input.mode {
            TextInputMode::NewServerProfile { .. } => 0,
            TextInputMode::EditServerParameter { field, .. } => field.len() + 1,
        }
    }

    pub fn text_input_value_len(&self) -> usize {
        self.text_input
            .as_ref()
            .map(|input| input.value.len())
            .unwrap_or(0)
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
            .filter(|e| e.ts >= start && e.ts <= now_ms)
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
    fn status_message_expires() {
        let mut app = App::new();
        app.dirty = false;
        app.set_status("updated policy");
        assert_eq!(app.status_message.as_deref(), Some("updated policy"));

        app.dirty = false;
        app.clear_expired_status(Instant::now() + STATUS_MESSAGE_TTL + Duration::from_millis(1));

        assert_eq!(app.status_message, None);
        assert!(app.dirty);
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
