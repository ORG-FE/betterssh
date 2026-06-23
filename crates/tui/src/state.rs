use crate::theme::Theme;
use std::collections::VecDeque;
use std::path::PathBuf;

use betterssh_core::{Host, Settings, Snippet};
use betterssh_ssh::{ClientHandler, ConnectOpts, RemoteForwards, SshEvent};
use russh::client::Handle as SshHandle;
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use crate::app::SessionCmd;
use crate::pty_render::TerminalView;

impl Default for SystemMetrics {
    fn default() -> Self {
        Self {
            cpu_pct: 0.0,
            cpu_cores: num_cpus(),
            ram_used_mb: 0,
            ram_total_mb: 0,
            disk_used_gb: 0.0,
            disk_total_gb: 0.0,
            net_up_kbs: 0.0,
            net_down_kbs: 0.0,
            uptime_secs: 0,
            load_1: 0.0,
            load_5: 0.0,
            load_15: 0.0,
        }
    }
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

pub type SessionId = u64;

pub struct SystemMetrics {
    pub cpu_pct: f32,
    pub cpu_cores: usize,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub disk_used_gb: f32,
    pub disk_total_gb: f32,
    pub net_up_kbs: f32,
    pub net_down_kbs: f32,
    pub uptime_secs: u64,
    pub load_1: f32,
    pub load_5: f32,
    pub load_15: f32,
}

pub type RemoteMetrics = SystemMetrics;

pub enum Focus {
    Hosts,
    Terminal,
    Sftp,
    Search,
    TermSearch,
    CmdPalette,
    Prompt,
    Settings,
    History,
}

#[derive(Clone)]
pub enum SessionStatus {
    Connecting,
    Active,
    Disconnected(String),
}

#[derive(Clone)]
pub struct SearchState {
    pub query: String,
    pub matches: Vec<(usize, usize)>,
    pub current: usize,
    pub active: bool,
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            matches: vec![],
            current: 0,
            active: false,
        }
    }

    pub fn update(&mut self, lines: &[Vec<char>]) {
        self.matches.clear();
        self.current = 0;
        if self.query.is_empty() {
            return;
        }
        for (i, line) in lines.iter().enumerate() {
            if line.len() < self.query.len() {
                continue;
            }
            let q: Vec<char> = self.query.chars().collect();
            for start in 0..=line.len().saturating_sub(q.len()) {
                if line[start..start + q.len()] == q[..] {
                    self.matches.push((i, start));
                }
            }
        }
    }
}

pub struct CmdHistory {
    pub entries: VecDeque<String>,
    pub capacity: usize,
    pub filter: String,
    pub matched: Vec<usize>,
    pub selected: usize,
}

impl CmdHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
            filter: String::new(),
            matched: Vec::new(),
            selected: 0,
        }
    }

    pub fn push(&mut self, cmd: String) {
        if cmd.trim().is_empty() {
            return;
        }
        if self.entries.back().map_or(false, |last| last == &cmd) {
            return;
        }
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(cmd);
    }

    pub fn update_filter(&mut self) {
        if self.filter.is_empty() {
            self.matched = (0..self.entries.len()).rev().collect();
        } else {
            let q = self.filter.to_lowercase();
            self.matched = self
                .entries
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, e)| e.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect();
        }
        self.selected = 0;
    }

    pub fn selected(&self) -> Option<&str> {
        self.matched
            .get(self.selected)
            .and_then(|&i| self.entries.get(i))
            .map(|s| s.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub struct Session {
    pub id: SessionId,
    pub host_name: String,
    pub label: String,
    pub handle: Option<SharedHandle>,
    pub cmd_tx: Option<mpsc::UnboundedSender<SessionCmd>>,
    pub events: mpsc::UnboundedReceiver<SshEvent>,
    pub view: TerminalView,
    pub tx_cols: u16,
    pub tx_rows: u16,
    pub mouse_active: bool,
    pub status: SessionStatus,
    pub sftp_state: Option<SftpState>,
    pub sftp_rx: Option<mpsc::UnboundedReceiver<Vec<SftpEntry>>>,
    pub sftp_result_rx: Option<mpsc::UnboundedReceiver<Result<(), String>>>,
    pub search: SearchState,
    pub history: CmdHistory,
    pub forwards: Vec<ActiveForward>,
    pub remote_forwards: Option<RemoteForwards>,
}

#[derive(Debug, Clone)]
pub struct ActiveForward {
    pub id: u64,
    pub direction: String,
    pub listen: String,
    pub target: String,
    pub active: bool,
    pub status: String,
}

pub enum AppMode {
    Browsing,
    Sftp,
    Prompt {
        kind: PromptKind,
        buffer: String,
        cursor: usize,
    },
    Message {
        text: String,
        level: MsgLevel,
        until: std::time::Instant,
    },
}

pub struct ActiveSftp {
    pub session_id: SessionId,
}

#[derive(Clone)]
pub enum PromptKind {
    Password {
        host: String,
    },
    Passphrase {
        path: String,
    },
    NewHost,
    DeleteConfirm {
        host: String,
    },
    EditField {
        host_name: String,
        field: EditField,
        original: String,
    },
    MasterPassword,

    JumpPassword {
        via: String,
        dest: String,
    },
    SftpMkdir {
        session_id: SessionId,
    },
    SftpRename {
        session_id: SessionId,
    },
    SftpFilter {
        session_id: SessionId,
    },
    RenameSession {
        session_idx: usize,
    },
    BatchCommand,
    KeybindingEdit {
        action: String,
        current: String,
    },
    KeybindingNew,
    MacroName {
        current: String,
    },
    MacroCmds {
        name: String,
        current_cmds: String,
    },
}

#[derive(Clone)]
pub enum EditField {
    Name,
    Host,
    Port,
    User,
    Group,
    Tags,
    KeyPath,
    JumpHost,
    Password,
    Keepalive,
    OnConnect,
    Forwards,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    Available,
    Downloading,
    Done,
    Failed(String),
}

pub enum MsgLevel {
    Info,
    Warn,
    Bad,
}

pub struct Toast {
    pub text: String,
    pub until: std::time::Instant,
    pub level: MsgLevel,
}

pub type SharedHandle = std::sync::Arc<AsyncMutex<SshHandle<ClientHandler>>>;

pub struct App {
    pub hosts: Vec<Host>,
    pub host_state: ratatui::widgets::ListState,
    pub filter: String,
    pub focus: Focus,
    pub mode: AppMode,
    pub should_quit: bool,
    pub term_rows: u16,
    pub term_cols: u16,
    pub status_msg: Option<String>,
    pub event_log: VecDeque<String>,
    pub toasts: VecDeque<Toast>,
    pub last_input: Vec<u8>,
    pub pending_resize: Option<(u16, u16)>,
    pub edit_target: Option<String>,
    pub snippets: Vec<Snippet>,
    pub session_log: VecDeque<String>,

    pub sessions: Vec<Session>,
    pub active_session: Option<usize>,
    pub next_session_id: SessionId,

    pub dial_tx: Option<mpsc::UnboundedSender<crate::app::DialResult>>,

    pub dial_session_id: Option<SessionId>,

    pub sftp_session_id: Option<SessionId>,

    pub master_vault: Option<betterssh_core::Vault>,
    pub master_password: Option<String>,
    pub last_entered_password: Option<String>,
    pub settings: Settings,
    pub theme: Theme,
    pub group_mode: bool,
    pub collapsed_groups: std::collections::HashSet<String>,
    pub capture_mode: bool,
    pub settings_focus: Option<super::settings::SettingsFocus>,
    pub settings_confirm_discard: bool,
    pub palette_filter: String,
    pub palette_selected: usize,

    pub batch_selected: std::collections::HashSet<String>,
    pub batch_results: Vec<BatchResult>,

    pub pending_macro_name: Option<(String, Option<usize>)>,

    pub pending_host_opts: Option<(String, ConnectOpts)>,
    pub pending_dial: Option<PendingDial>,
    pub metrics: SystemMetrics,
    pub prev_net_rx: u64,
    pub prev_net_tx: u64,
    pub prev_net_time: std::time::Instant,
    pub prev_cpu_work: u64,
    pub prev_cpu_idle: u64,
    pub host_status: std::collections::HashMap<String, HostStatus>,
    pub last_host_check: std::time::Instant,
    pub pending_host_checks: Vec<(
        String,
        std::sync::mpsc::Receiver<std::result::Result<(), String>>,
    )>,

    pub remote_metrics: Option<RemoteMetrics>,
    pub remote_metrics_rx: Option<tokio::sync::oneshot::Receiver<Option<RemoteMetrics>>>,
    pub last_remote_metrics_collect: std::time::Instant,

    pub sftp_rx: Option<mpsc::UnboundedReceiver<Vec<SftpEntry>>>,
    pub sftp_result_rx: Option<mpsc::UnboundedReceiver<Result<(), String>>>,
    pub batch_rx: Option<mpsc::UnboundedReceiver<BatchResult>>,

    pub update_status: UpdateStatus,
    pub update_latest_version: String,
    pub update_error: Option<String>,
    pub update_dismissed: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub enum HostStatus {
    Unknown,
    Alive,
    Dead(String),
}

#[derive(Clone)]
pub enum BatchResult {
    Pending { host: String },
    Success { host: String, output: String },
    Failed { host: String, error: String },
}

impl BatchResult {
    pub fn host(&self) -> &str {
        match self {
            BatchResult::Pending { host }
            | BatchResult::Success { host, .. }
            | BatchResult::Failed { host, .. } => host,
        }
    }
}

pub struct PendingDial {
    pub host_name: String,
    pub pw_tx: mpsc::UnboundedSender<String>,
    pub session_id: SessionId,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SftpPane {
    Local,
    Remote,
}

pub enum PanePath<'a> {
    Local(&'a PathBuf),
    Remote(&'a String),
}

impl<'a> PanePath<'a> {
    pub fn display(&self) -> String {
        match self {
            PanePath::Local(p) => p.display().to_string(),
            PanePath::Remote(s) => s.to_string(),
        }
    }
}

#[derive(Clone)]
pub struct SftpEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Clone)]
pub struct SftpState {
    pub local_path: PathBuf,
    pub remote_path: String,
    pub local_entries: Vec<SftpEntry>,
    pub remote_entries: Vec<SftpEntry>,
    pub focus: SftpPane,
    pub sel: usize,
    pub filter: String,
    pub local_loading: bool,
    pub remote_loading: bool,
    pub local_err: Option<String>,
    pub remote_err: Option<String>,
}

impl SftpState {
    pub fn new(local_path: PathBuf) -> Self {
        Self {
            local_path,
            remote_path: "/".into(),
            local_entries: Vec::new(),
            remote_entries: Vec::new(),
            focus: SftpPane::Local,
            sel: 0,
            filter: String::new(),
            local_loading: false,
            remote_loading: false,
            local_err: None,
            remote_err: None,
        }
    }

    pub fn with_selection(local_path: PathBuf, sel: usize) -> Self {
        Self {
            sel,
            ..Self::new(local_path)
        }
    }

    pub fn with_entries(
        local_path: PathBuf,
        local: Vec<SftpEntry>,
        remote: Vec<SftpEntry>,
    ) -> Self {
        Self {
            local_path,
            remote_path: "/".into(),
            local_entries: local,
            remote_entries: remote,
            focus: SftpPane::Local,
            sel: 0,
            filter: String::new(),
            local_loading: false,
            remote_loading: false,
            local_err: None,
            remote_err: None,
        }
    }

    pub fn pane_path(&self, pane: SftpPane) -> PanePath<'_> {
        match pane {
            SftpPane::Local => PanePath::Local(&self.local_path),
            SftpPane::Remote => PanePath::Remote(&self.remote_path),
        }
    }

    pub fn set_path(&mut self, pane: SftpPane, path: String) {
        match pane {
            SftpPane::Local => {
                self.local_path = PathBuf::from(path);
            }
            SftpPane::Remote => {
                self.remote_path = path;
            }
        }
    }

    pub fn current_entries(&self) -> &[SftpEntry] {
        match self.focus {
            SftpPane::Local => &self.local_entries,
            SftpPane::Remote => &self.remote_entries,
        }
    }

    pub fn move_sel(&mut self, delta: i32) {
        let n = self.current_entries().len();
        if n == 0 {
            self.sel = 0;
            return;
        }
        let cur = self.sel as i32;
        let mut next = cur + delta;
        if next < 0 {
            next = 0;
        }
        if next >= n as i32 {
            next = n as i32 - 1;
        }
        self.sel = next as usize;
    }

    pub fn refresh_local(&mut self) {
        self.local_entries.clear();
        let read = std::fs::read_dir(&self.local_path);
        match read {
            Ok(rd) => {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    let meta = e.metadata().ok();
                    let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    self.local_entries.push(SftpEntry { name, is_dir, size });
                }
                self.local_err = None;
            }
            Err(e) => {
                self.local_err = Some(format!("{}", e));
            }
        }
        self.local_entries
            .sort_by(|a, b| match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.cmp(&b.name),
            });
    }
}

impl Session {
    pub fn new(id: SessionId, host_name: String, label: String, cols: u16, rows: u16) -> Self {
        Self {
            id,
            host_name,
            label,
            handle: None,
            cmd_tx: None,
            events: mpsc::unbounded_channel().1,
            view: TerminalView::new(cols.max(20), rows.max(5)),
            tx_cols: cols.max(20),
            tx_rows: rows.max(5),
            mouse_active: false,
            status: SessionStatus::Connecting,
            sftp_state: None,
            sftp_rx: None,
            sftp_result_rx: None,
            search: SearchState::new(),
            history: CmdHistory::new(500),
            forwards: Vec::new(),
            remote_forwards: None,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status, SessionStatus::Active)
    }

    pub fn disconnected(&self) -> Option<&str> {
        if let SessionStatus::Disconnected(reason) = &self.status {
            Some(reason.as_str())
        } else {
            None
        }
    }
}

impl App {
    pub fn new(hosts: Vec<Host>, cols: u16, rows: u16, snippets: Vec<Snippet>) -> Self {
        let mut state = ratatui::widgets::ListState::default();
        if !hosts.is_empty() {
            state.select(Some(0));
        }
        Self {
            hosts,
            host_state: state,
            filter: String::new(),
            focus: Focus::Prompt,
            mode: AppMode::Prompt {
                kind: PromptKind::MasterPassword,
                buffer: String::new(),
                cursor: 0,
            },
            should_quit: false,
            term_cols: cols,

            term_rows: rows,
            status_msg: Some("Enter master password".into()),
            event_log: VecDeque::with_capacity(64),
            toasts: VecDeque::with_capacity(16),
            last_input: Vec::with_capacity(64),
            pending_resize: None,
            edit_target: None,
            snippets,
            session_log: VecDeque::with_capacity(64),

            sessions: Vec::new(),
            active_session: None,
            next_session_id: 1,

            dial_tx: None,
            dial_session_id: None,

            sftp_session_id: None,

            master_vault: None,
            master_password: None,
            last_entered_password: None,
            settings: Settings::default(),
            theme: Theme::default(),
            group_mode: true,
            collapsed_groups: std::collections::HashSet::new(),
            capture_mode: false,
            palette_filter: String::new(),
            palette_selected: 0,
            batch_selected: std::collections::HashSet::new(),
            batch_results: Vec::new(),
            settings_focus: None,
            settings_confirm_discard: false,

            pending_host_opts: None,
            pending_macro_name: None,
            pending_dial: None,
            metrics: SystemMetrics::default(),
            prev_net_rx: 0,
            prev_net_tx: 0,
            prev_net_time: std::time::Instant::now(),
            prev_cpu_work: 0,
            prev_cpu_idle: 0,
            host_status: std::collections::HashMap::new(),
            last_host_check: std::time::Instant::now() - std::time::Duration::from_secs(60),
            pending_host_checks: Vec::new(),
            remote_metrics: None,
            remote_metrics_rx: None,
            last_remote_metrics_collect: std::time::Instant::now()
                - std::time::Duration::from_secs(10),
            sftp_rx: None,
            sftp_result_rx: None,
            batch_rx: None,

            update_status: UpdateStatus::Idle,
            update_latest_version: String::new(),
            update_error: None,
            update_dismissed: false,
        }
    }

    pub fn alloc_session_id(&mut self) -> SessionId {
        let id = self.next_session_id;
        self.next_session_id += 1;
        id
    }

    pub fn session_index(&self, id: SessionId) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    pub fn active_session_ref(&self) -> Option<&Session> {
        self.active_session.and_then(|i| self.sessions.get(i))
    }

    pub fn active_session_mut(&mut self) -> Option<&mut Session> {
        let i = self.active_session?;
        self.sessions.get_mut(i)
    }

    pub fn list_to_host_idx(&self, list_sel: usize) -> Option<usize> {
        let filtered = self.filtered_indices();
        if self.group_mode {
            let hosts = &self.hosts;
            let mut current_group: Option<&str> = None;
            let mut list_pos: usize = 0;
            for h_idx in &filtered {
                let h = &hosts[*h_idx];
                let grp = h.group.as_deref().unwrap_or("ungrouped");
                if current_group != Some(grp) {
                    if list_pos == list_sel {
                        return None;
                    }
                    list_pos += 1;
                    current_group = Some(grp);
                }
                if list_pos == list_sel {
                    return Some(*h_idx);
                }
                list_pos += 1;
            }
            None
        } else {
            filtered.get(list_sel).copied()
        }
    }

    pub fn selected_host(&self) -> Option<&Host> {
        let idx = self.host_state.selected()?;
        let real_idx = self.list_to_host_idx(idx)?;
        Some(&self.hosts[real_idx])
    }

    pub fn selected_host_mut(&mut self) -> Option<&mut Host> {
        let idx = self.host_state.selected()?;
        let real_idx = self.list_to_host_idx(idx)?;
        Some(&mut self.hosts[real_idx])
    }

    pub fn find_host(&self, name: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.name == name)
    }

    pub fn find_host_mut(&mut self, name: &str) -> Option<&mut Host> {
        self.hosts.iter_mut().find(|h| h.name == name)
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() && !self.group_mode {
            return (0..self.hosts.len()).collect();
        }
        let q = self.filter.to_lowercase();
        self.hosts
            .iter()
            .enumerate()
            .filter(|(_, h)| {
                if self.group_mode {
                    let grp = h.group.as_deref().unwrap_or("ungrouped");
                    if self.collapsed_groups.contains(grp) {
                        return false;
                    }
                }
                if self.filter.is_empty() {
                    return true;
                }
                h.name.to_lowercase().contains(&q)
                    || h.host.to_lowercase().contains(&q)
                    || h.user.to_lowercase().contains(&q)
                    || h.group
                        .as_deref()
                        .map(|g| g.to_lowercase().contains(&q))
                        .unwrap_or(false)
                    || h.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn move_selection(&mut self, delta: i32) {
        let n = self.filtered_indices().len();
        if n == 0 {
            return;
        }
        if !self.group_mode {
            let cur = self.host_state.selected().unwrap_or(0) as i32;
            let mut next = cur + delta;
            if next < 0 {
                next = 0;
            }
            if next >= n as i32 {
                next = n as i32 - 1;
            }
            self.host_state.select(Some(next as usize));
            return;
        }

        let total = self.group_list_len();
        if total == 0 {
            return;
        }
        let cur = self.host_state.selected().unwrap_or(0) as i32;
        let mut next = cur;
        for _ in 0..total {
            next += delta;
            if next < 0 {
                next = total as i32 - 1;
            }
            if next >= total as i32 {
                next = 0;
            }
            if self.list_to_host_idx(next as usize).is_some() {
                self.host_state.select(Some(next as usize));
                return;
            }
        }
    }

    fn group_list_len(&self) -> usize {
        let mut len = 0;
        let mut current_group: Option<&str> = None;
        let filtered = self.filtered_indices();
        for h_idx in &filtered {
            let h = &self.hosts[*h_idx];
            let grp = h.group.as_deref().unwrap_or("ungrouped");
            if current_group != Some(grp) {
                len += 1;
                current_group = Some(grp);
            }
            len += 1;
        }
        len
    }

    pub fn cycle_session(&mut self, dir: i32) {
        let n = self.sessions.len();
        if n == 0 {
            return;
        }
        let cur = self.active_session.unwrap_or(0) as i32;
        let mut next = cur + dir;

        if next < 0 {
            next = n as i32 - 1;
        } else if next >= n as i32 {
            next = 0;
        }
        self.active_session = Some(next as usize);
    }

    pub fn push_log(&mut self, line: impl Into<String>) {
        if self.event_log.len() == 64 {
            self.event_log.pop_front();
        }
        self.event_log.push_back(line.into());
    }

    pub fn push_toast(&mut self, text: impl Into<String>, level: MsgLevel) {
        if self.toasts.len() == 8 {
            self.toasts.pop_front();
        }
        self.toasts.push_back(Toast {
            text: text.into(),
            until: std::time::Instant::now() + std::time::Duration::from_secs(4),
            level,
        });
    }

    pub fn mouse_active_now(&self) -> bool {
        self.active_session_ref()
            .map(|s| s.mouse_active)
            .unwrap_or(false)
    }

    pub fn save_config(&self, settings: &Settings) {
        let cfg = betterssh_core::Config {
            host: self.hosts.clone(),
            settings: settings.clone(),
            snippets: self.snippets.clone(),
        };
        if let Err(e) = betterssh_core::save(betterssh_core::config_path().unwrap(), &cfg) {
            tracing::error!("save config: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_hosts() -> Vec<betterssh_core::Host> {
        vec![
            betterssh_core::Host {
                name: "web".into(),
                host: "192.168.1.1".into(),
                port: 22,
                user: "root".into(),
                identity: vec![],
                jump: None,
                tags: vec!["prod".into(), "web".into()],
                group: Some("production".into()),
                keepalive: None,
                on_connect: vec![],
                forwarding: vec![],
            },
            betterssh_core::Host {
                name: "db".into(),
                host: "192.168.1.2".into(),
                port: 5432,
                user: "postgres".into(),
                identity: vec![],
                jump: None,
                tags: vec!["prod".into(), "db".into()],
                group: Some("production".into()),
                keepalive: None,
                on_connect: vec![],
                forwarding: vec![],
            },
            betterssh_core::Host {
                name: "devbox".into(),
                host: "10.0.0.1".into(),
                port: 22,
                user: "dev".into(),
                identity: vec![],
                jump: None,
                tags: vec!["dev".into()],
                group: Some("development".into()),
                keepalive: None,
                on_connect: vec![],
                forwarding: vec![],
            },
            betterssh_core::Host {
                name: "nas".into(),
                host: "10.0.0.2".into(),
                port: 22,
                user: "admin".into(),
                identity: vec![],
                jump: None,
                tags: vec![],
                group: None,
                keepalive: None,
                on_connect: vec![],
                forwarding: vec![],
            },
        ]
    }

    fn make_test_app() -> App {
        let hosts = make_test_hosts();
        let mut app = App::new(hosts, 120, 32, vec![]);
        app.host_state.select(Some(0));
        app
    }

    // ===== SearchState =====

    #[test]
    fn search_state_new() {
        let s = SearchState::new();
        assert!(s.query.is_empty());
        assert!(s.matches.is_empty());
        assert_eq!(s.current, 0);
        assert!(!s.active);
    }

    #[test]
    fn search_state_update_empty_query() {
        let mut s = SearchState::new();
        let lines = vec![vec!['a', 'b', 'c'], vec!['d', 'e', 'f']];
        s.update(&lines);
        assert!(s.matches.is_empty());
    }

    #[test]
    fn search_state_update_finds_matches() {
        let mut s = SearchState::new();
        s.query = "ab".into();
        let lines = vec![
            "abc".chars().collect(),
            "def".chars().collect(),
            "xabx".chars().collect(),
        ];
        s.update(&lines);
        assert_eq!(s.matches.len(), 2);
        assert!(s.matches.contains(&(0, 0)));
        assert!(s.matches.contains(&(2, 1)));
    }

    #[test]
    fn search_state_update_no_matches() {
        let mut s = SearchState::new();
        s.query = "zzz".into();
        let lines = vec!["abc".chars().collect(), "def".chars().collect()];
        s.update(&lines);
        assert!(s.matches.is_empty());
    }

    #[test]
    fn search_state_update_multi_byte() {
        let mut s = SearchState::new();
        s.query = "при".into();
        let lines = vec!["привет".chars().collect(), "мир".chars().collect()];
        s.update(&lines);
        assert_eq!(s.matches.len(), 1);
        assert_eq!(s.matches[0], (0, 0));
    }

    // ===== SftpState =====

    #[test]
    fn sftp_state_new() {
        let s = SftpState::new("/tmp".into());
        assert_eq!(s.local_path, PathBuf::from("/tmp"));
        assert_eq!(s.remote_path, "/");
        assert_eq!(s.focus, SftpPane::Local);
        assert_eq!(s.sel, 0);
    }

    #[test]
    fn sftp_state_set_path() {
        let mut s = SftpState::new("/tmp".into());
        s.set_path(SftpPane::Remote, "/home".into());
        assert_eq!(s.remote_path, "/home");
        s.set_path(SftpPane::Local, "/var".into());
        assert_eq!(s.local_path, PathBuf::from("/var"));
    }

    #[test]
    fn sftp_state_current_entries_focus() {
        let local = vec![SftpEntry { name: "a".into(), is_dir: false, size: 10 }];
        let remote = vec![SftpEntry { name: "b".into(), is_dir: true, size: 0 }];
        let mut s = SftpState::with_entries("/tmp".into(), local.clone(), remote.clone());
        assert_eq!(s.current_entries().len(), 1);
        assert_eq!(s.current_entries()[0].name, "a");

        s.focus = SftpPane::Remote;
        assert_eq!(s.current_entries().len(), 1);
        assert_eq!(s.current_entries()[0].name, "b");
    }

    #[test]
    fn sftp_state_move_sel() {
        let local = vec![
            SftpEntry { name: "a".into(), is_dir: false, size: 0 },
            SftpEntry { name: "b".into(), is_dir: false, size: 0 },
            SftpEntry { name: "c".into(), is_dir: false, size: 0 },
        ];
        let mut s = SftpState::with_entries("/tmp".into(), local, vec![]);
        assert_eq!(s.sel, 0);
        s.move_sel(1);
        assert_eq!(s.sel, 1);
        s.move_sel(1);
        assert_eq!(s.sel, 2);
        s.move_sel(1);
        assert_eq!(s.sel, 2);
        s.move_sel(-1);
        assert_eq!(s.sel, 1);
        s.move_sel(-10);
        assert_eq!(s.sel, 0);
    }

    #[test]
    fn sftp_state_move_sel_empty() {
        let mut s = SftpState::new("/tmp".into());
        s.move_sel(1);
        assert_eq!(s.sel, 0);
    }

    #[test]
    fn sftp_state_refresh_local_valid() {
        let tmp = std::env::temp_dir().join("betterssh_sftp_test");
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("test.txt"), b"hello").unwrap();
        std::fs::create_dir(tmp.join("subdir")).unwrap();

        let mut s = SftpState::new(tmp.clone());
        s.refresh_local();
        assert!(s.local_err.is_none());

        let names: Vec<String> = s.local_entries.iter().map(|e| e.name.clone()).collect();
        assert!(names.contains(&"test.txt".into()));
        assert!(names.contains(&"subdir".into()));

        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn sftp_state_refresh_local_invalid() {
        let mut s = SftpState::new("/nonexistent_path_xyz".into());
        s.refresh_local();
        assert!(s.local_err.is_some());
    }

    #[test]
    fn sftp_state_with_selection() {
        let s = SftpState::with_selection("/tmp".into(), 5);
        assert_eq!(s.sel, 5);
    }

    // ===== Session =====

    #[test]
    fn session_new() {
        let sess = Session::new(1, "test".into(), "root@1.2.3.4".into(), 80, 24);
        assert_eq!(sess.id, 1);
        assert_eq!(sess.host_name, "test");
        assert_eq!(sess.label, "root@1.2.3.4");
        assert!(matches!(sess.status, SessionStatus::Connecting));
        assert!(!sess.mouse_active);
    }

    #[test]
    fn session_is_active() {
        let mut sess = Session::new(1, "".into(), "".into(), 80, 24);
        assert!(!sess.is_active());
        sess.status = SessionStatus::Active;
        assert!(sess.is_active());
        sess.status = SessionStatus::Disconnected("err".into());
        assert!(!sess.is_active());
    }

    #[test]
    fn session_disconnected() {
        let mut sess = Session::new(1, "".into(), "".into(), 80, 24);
        assert!(sess.disconnected().is_none());
        sess.status = SessionStatus::Disconnected("timeout".into());
        assert_eq!(sess.disconnected(), Some("timeout"));
    }

    // ===== App =====

    #[test]
    fn app_new() {
        let hosts = make_test_hosts();
        let app = App::new(hosts.clone(), 100, 30, vec![]);
        assert_eq!(app.hosts.len(), 4);
        assert_eq!(app.term_cols, 100);
        assert_eq!(app.term_rows, 30);
        assert!(matches!(app.mode, AppMode::Prompt { .. }));
        assert!(app.sessions.is_empty());
        assert!(app.active_session.is_none());
        assert!(!app.should_quit);
    }

    #[test]
    fn app_alloc_session_id() {
        let mut app = make_test_app();
        assert_eq!(app.alloc_session_id(), 1);
        assert_eq!(app.alloc_session_id(), 2);
        assert_eq!(app.next_session_id, 3);
    }

    #[test]
    fn app_session_index() {
        let mut app = make_test_app();
        let id1 = app.alloc_session_id();
        let id2 = app.alloc_session_id();
        app.sessions.push(Session::new(id1, "a".into(), "".into(), 80, 24));
        app.sessions.push(Session::new(id2, "b".into(), "".into(), 80, 24));
        assert_eq!(app.session_index(id1), Some(0));
        assert_eq!(app.session_index(id2), Some(1));
        assert_eq!(app.session_index(999), None);
    }

    #[test]
    fn app_filtered_indices_no_filter() {
        let mut app = make_test_app();
        app.group_mode = false;
        let indices = app.filtered_indices();
        assert_eq!(indices.len(), 4);
    }

    #[test]
    fn app_filtered_indices_by_name() {
        let mut app = make_test_app();
        app.group_mode = false;
        app.filter = "web".into();
        let indices = app.filtered_indices();
        assert_eq!(indices.len(), 1);
        assert_eq!(app.hosts[indices[0]].name, "web");
    }

    #[test]
    fn app_filtered_indices_by_host() {
        let mut app = make_test_app();
        app.group_mode = false;
        app.filter = "192.168".into();
        let indices = app.filtered_indices();
        assert_eq!(indices.len(), 2);
    }

    #[test]
    fn app_filtered_indices_by_tag() {
        let mut app = make_test_app();
        app.group_mode = false;
        app.filter = "db".into();
        let indices = app.filtered_indices();
        assert_eq!(indices.len(), 1);
        assert_eq!(app.hosts[indices[0]].name, "db");
    }

    #[test]
    fn app_filtered_indices_no_match() {
        let mut app = make_test_app();
        app.group_mode = false;
        app.filter = "zzz".into();
        assert!(app.filtered_indices().is_empty());
    }

    #[test]
    fn app_filtered_indices_group_mode_collapsed() {
        let mut app = make_test_app();
        app.group_mode = true;
        app.filter = "".into();
        app.collapsed_groups.insert("production".into());
        let indices = app.filtered_indices();
        assert!(!indices.iter().any(|&i| app.hosts[i].group.as_deref() == Some("production")));
        assert!(indices.iter().any(|&i| app.hosts[i].group.as_deref() == Some("development") || app.hosts[i].group.is_none()));
    }

    #[test]
    fn app_find_host() {
        let app = make_test_app();
        assert!(app.find_host("web").is_some());
        assert!(app.find_host("nonexistent").is_none());
    }

    #[test]
    fn app_find_host_mut() {
        let mut app = make_test_app();
        let h = app.find_host_mut("web").unwrap();
        h.port = 2222;
        assert_eq!(app.hosts[0].port, 2222);
    }

    #[test]
    fn app_selected_host() {
        let mut app = make_test_app();
        app.group_mode = false;
        app.host_state.select(Some(0));
        let h = app.selected_host();
        assert!(h.is_some());
        assert_eq!(h.unwrap().name, "web");
    }

    #[test]
    fn app_selected_host_mut() {
        let mut app = make_test_app();
        app.group_mode = false;
        app.host_state.select(Some(1));
        let h = app.selected_host_mut().unwrap();
        h.user = "changed".into();
        assert_eq!(app.hosts[1].user, "changed");
    }

    #[test]
    fn app_move_selection() {
        let mut app = make_test_app();
        app.group_mode = false;
        app.host_state.select(Some(0));
        app.move_selection(1);
        assert_eq!(app.host_state.selected(), Some(1));
        app.move_selection(2);
        assert_eq!(app.host_state.selected(), Some(3));
        app.move_selection(10);
        assert_eq!(app.host_state.selected(), Some(3));
        app.move_selection(-10);
        assert_eq!(app.host_state.selected(), Some(0));
    }

    #[test]
    fn app_move_selection_empty() {
        let mut app = App::new(vec![], 80, 24, vec![]);
        app.move_selection(1);
        assert!(app.host_state.selected().is_none());
    }

    #[test]
    fn app_list_to_host_idx_grouped() {
        let mut app = make_test_app();
        app.group_mode = true;
        let idx = app.list_to_host_idx(1);
        assert!(idx.is_some());
    }

    #[test]
    fn app_cycle_session() {
        let mut app = make_test_app();
        app.sessions.push(Session::new(1, "a".into(), "".into(), 80, 24));
        app.sessions.push(Session::new(2, "b".into(), "".into(), 80, 24));
        app.active_session = Some(0);
        app.cycle_session(1);
        assert_eq!(app.active_session, Some(1));
        app.cycle_session(1);
        assert_eq!(app.active_session, Some(0));
        app.cycle_session(-1);
        assert_eq!(app.active_session, Some(1));
    }

    #[test]
    fn app_cycle_session_empty() {
        let mut app = make_test_app();
        app.cycle_session(1);
        assert!(app.active_session.is_none());
    }

    #[test]
    fn app_push_log() {
        let mut app = make_test_app();
        app.push_log("line1");
        app.push_log("line2");
        assert_eq!(app.event_log.len(), 2);
        assert_eq!(app.event_log[0], "line1");
        assert_eq!(app.event_log[1], "line2");
    }

    #[test]
    fn app_push_log_capacity() {
        let mut app = make_test_app();
        for i in 0..70 {
            app.push_log(format!("line{}", i));
        }
        assert_eq!(app.event_log.len(), 64);
        assert_eq!(app.event_log[0], "line6");
    }

    #[test]
    fn app_push_toast() {
        let mut app = make_test_app();
        app.push_toast("hello", MsgLevel::Info);
        app.push_toast("warning", MsgLevel::Warn);
        app.push_toast("error", MsgLevel::Bad);
        assert_eq!(app.toasts.len(), 3);
        assert_eq!(app.toasts[0].text, "hello");
        assert!(matches!(app.toasts[0].level, MsgLevel::Info));
    }

    #[test]
    fn app_push_toast_capacity() {
        let mut app = make_test_app();
        for i in 0..10 {
            app.push_toast(format!("t{}", i), MsgLevel::Info);
        }
        assert_eq!(app.toasts.len(), 8);
    }

    #[test]
    fn app_mouse_active_now() {
        let mut app = make_test_app();
        assert!(!app.mouse_active_now());
        let id = app.alloc_session_id();
        app.sessions.push(Session::new(id, "".into(), "".into(), 80, 24));
        app.active_session = Some(0);
        app.sessions[0].mouse_active = true;
        assert!(app.mouse_active_now());
    }

    #[test]
    fn app_alloc_session_id_unique() {
        let mut app = make_test_app();
        let ids: Vec<u64> = (0..100).map(|_| app.alloc_session_id()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(ids.len(), sorted.len());
    }
}
