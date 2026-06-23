use crate::pty_render::TerminalView;
use crate::settings::{draw_settings, SettingsAction, SettingsFocus};
use crate::state::{
    ActiveForward, App, AppMode, EditField, Focus, HostStatus, MsgLevel, PendingDial, PromptKind,
    RemoteMetrics, Session, SessionStatus, SftpEntry, SftpPane, SftpState, UpdateStatus,
};
use crate::theme::{self, Theme};
use crate::update;
use crate::view::{
    draw_host_list, draw_prompt, draw_sftp, draw_status_bar, draw_toasts, popup_area,
};
use anyhow::Result;
use betterssh_core::{
    entries_to_hosts, host_id, merge_hosts, parse_ssh_config, ssh_config_path, vault_create,
    vault_exists, vault_load, ForwardDirection, Host, Identity, PortForward, Settings, Snippet,
};
use betterssh_ssh::{
    exec, open_shell, AuthChoice, ClientHandler, ConnectOpts, RemoteForwards, RemoteFs, SshEvent,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use russh::client::Handle as SshHandle;
use russh::ChannelMsg;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex as AsyncMutex};

pub async fn run(hosts: Vec<Host>, settings: Settings, snippets: Vec<Snippet>) -> Result<()> {
    let terminal = ratatui::init();
    let result = App::new(hosts, 120, 32, snippets)
        .run(terminal, settings)
        .await;
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    result
}

pub enum SessionCmd {
    Send(Vec<u8>),
    Resize(u16, u16),
    Stop,
}

type SharedHandle = Arc<AsyncMutex<SshHandle<ClientHandler>>>;

pub enum DialResult {
    Done(SharedHandle, RemoteForwards, String, ConnectOpts),
    Failed(String),
}

impl App {
    pub async fn run(mut self, mut terminal: DefaultTerminal, settings: Settings) -> Result<()> {
        self.settings = settings.clone();
        self.theme = theme::load_theme(&settings.theme);
        update::check_latest();
        let tick = Duration::from_millis(50);
        let mut last = Instant::now();
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Cmd>();
        let (dial_tx, mut dial_rx) = mpsc::unbounded_channel::<DialResult>();
        let settings_arc = Arc::new(settings);
        self.dial_tx = Some(dial_tx);

        loop {
            if self.should_quit {
                self.shutdown_all_sessions().await;
                break;
            }

            let now = Instant::now();
            if now.duration_since(last) >= tick {
                last = now;
                self.on_tick();
            }

            while let Ok(res) = dial_rx.try_recv() {
                self.on_dial_result(res);
            }

            self.draw(&mut terminal);

            if crossterm::event::poll(Duration::from_millis(20))? {
                if let Ok(evt) = crossterm::event::read() {
                    self.handle_event(evt, &cmd_tx, &settings_arc);
                }
            }

            while let Ok(cmd) = cmd_rx.try_recv() {
                self.handle_cmd(cmd, &settings_arc).await;
            }
        }

        Ok(())
    }

    fn on_dial_result(&mut self, res: DialResult) {
        let session_id = match self.dial_session_id.take() {
            Some(id) => id,
            None => return,
        };
        let idx = match self.session_index(session_id) {
            Some(i) => i,
            None => return,
        };
        match res {
            DialResult::Done(handle, rf, host_name, opts) => {
                let session_id_str = host_name.clone();

                if let (Some(pwd), Some(vault)) = (
                    self.last_entered_password.take(),
                    self.master_vault.as_mut(),
                ) {
                    let host_id_str = host_id(&opts.host, &opts.user);
                    if vault.get(&host_id_str).is_some() {
                        let mut secret = vault.get(&host_id_str).cloned().unwrap();
                        secret.password = pwd.clone();
                        vault.set(&host_id_str, secret);
                        if let Some(master_pwd) = &self.master_password {
                            if let Err(e) = betterssh_core::vault_save(vault, master_pwd) {
                                self.status_msg = Some(format!("vault save error: {}", e));
                            }
                        }
                    }
                }
                self.spawn_shell_session(idx, handle, rf, opts, host_name);
                self.push_toast(format!("connected {}", session_id_str), MsgLevel::Info);
            }
            DialResult::Failed(reason) => {
                self.sessions[idx].status =
                    SessionStatus::Disconnected(format!("connect failed: {}", reason));
                self.push_toast(format!("connect failed: {}", reason), MsgLevel::Bad);

                if self.active_session == Some(idx)
                    && self.sessions.iter().all(|s| {
                        matches!(
                            s.status,
                            SessionStatus::Disconnected(_) | SessionStatus::Connecting
                        )
                    })
                {
                    self.active_session = None;
                    self.focus = Focus::Hosts;
                }
            }
        }
    }

    fn spawn_shell_session(
        &mut self,
        idx: usize,
        handle: SharedHandle,
        rf: RemoteForwards,
        opts: ConnectOpts,
        host_name: String,
    ) {
        let (event_tx, event_rx) = mpsc::unbounded_channel::<SshEvent>();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCmd>();

        let (real_cols, real_rows) = crossterm::terminal::size().unwrap_or((120, 32));
        let pty_cols = real_cols.saturating_sub(2).max(20);
        let pty_rows = real_rows.saturating_sub(4).max(5);

        let handle_for_task = handle.clone();
        let opts_for_task = opts.clone();
        tokio::spawn(async move {
            let ch = {
                let h = handle_for_task.lock().await;
                match open_shell(&h, &opts_for_task).await {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = event_tx.send(SshEvent::Error(format!("shell: {}", e)));
                        return;
                    }
                }
            };
            run_shell_loop(ch, cmd_rx, event_tx).await;
        });

        let label = format!("{}@{}:{}", opts.user, opts.host, opts.port);
        let sess = &mut self.sessions[idx];
        sess.host_name = host_name.clone();
        sess.label = label;
        let _ = opts;
        sess.cmd_tx = Some(cmd_tx);
        sess.events = event_rx;
        sess.handle = Some(handle);
        sess.remote_forwards = Some(rf);
        sess.tx_cols = pty_cols;
        sess.tx_rows = pty_rows;
        sess.view = TerminalView::new(pty_cols, pty_rows);
        sess.status = SessionStatus::Active;

        if let Some(tx) = sess.cmd_tx.as_ref() {
            let _ = tx.send(SessionCmd::Resize(pty_cols, pty_rows));
        }

        if let Some(h) = self.hosts.iter().find(|h| h.name == host_name) {
            for cmd in &h.on_connect {
                if let Some(tx) = sess.cmd_tx.as_ref() {
                    let mut bytes = cmd.as_bytes().to_vec();
                    bytes.push(b'\n');
                    let _ = tx.send(SessionCmd::Send(bytes));
                }
            }
        }

        self.active_session = Some(idx);
        self.focus = Focus::Terminal;
    }

    async fn close_session_by_index(&mut self, idx: usize) {
        if idx >= self.sessions.len() {
            return;
        }

        if let Some(tx) = self.sessions[idx].cmd_tx.as_ref() {
            let _ = tx.send(SessionCmd::Send(b"\x1b[?1003l\x1b[?1006l".to_vec()));
            let _ = tx.send(SessionCmd::Stop);
        }
        self.sessions.remove(idx);
    }

    async fn shutdown_all_sessions(&mut self) {
        let count = self.sessions.len();
        for _ in 0..count {
            self.close_session_by_index(0).await;
        }
    }

    fn draw(&mut self, terminal: &mut DefaultTerminal) {
        let theme = self.theme.clone();
        let _ = terminal.draw(|f| self.render_frame(f, &theme));
    }

    fn render_frame(&mut self, f: &mut Frame, theme: &Theme) {
        let area = f.area();

        if let Some(idx) = self.active_session {
            if let Some(s) = self.sessions.get(idx) {
                if matches!(s.status, SessionStatus::Active | SessionStatus::Connecting)
                    || s.disconnected().is_some()
                {
                    self.render_connected(f, area, theme, idx);

                    if let Some(sf) = &mut self.settings_focus {
                        draw_settings(f, area, sf, theme, self.settings_confirm_discard);
                    }
                    if let AppMode::Prompt {
                        kind,
                        buffer,
                        cursor,
                    } = &self.mode
                    {
                        self.render_prompt_overlay(f, area, theme, kind, buffer, *cursor);
                    }
                    if matches!(self.focus, Focus::CmdPalette) {
                        self.render_palette(f, area, theme);
                    }
                    self.render_update_banner(f, area, theme);
                    return;
                }
            }
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);

        let body = chunks[0];
        let status_bar_area = chunks[1];

        let body_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(34), Constraint::Min(20)])
            .split(body);

        let hosts_focused = matches!(self.focus, Focus::Hosts | Focus::Search);
        draw_host_list(
            f,
            body_chunks[0],
            theme,
            &self.hosts,
            &mut self.host_state,
            &self.filter,
            hosts_focused,
            &self.host_status,
            self.group_mode,
            &self.collapsed_groups,
        );

        let term_area = body_chunks[1];
        match &self.mode {
            AppMode::Browsing => {
                let lines = self.render_details(theme);
                let p = Paragraph::new(lines).block(
                    ratatui::widgets::Block::default()
                        .borders(ratatui::widgets::Borders::ALL)
                        .border_type(ratatui::widgets::BorderType::Rounded)
                        .border_style(ratatui::style::Style::default().fg(theme.border2))
                        .title(ratatui::text::Span::styled(
                            " DETAILS ",
                            ratatui::style::Style::default()
                                .fg(theme.dim)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ))
                        .style(ratatui::style::Style::default().bg(theme.panel)),
                );
                f.render_widget(p, term_area);
            }
            AppMode::Message { text, level, .. } => {
                let color = match level {
                    MsgLevel::Info => theme.accent,
                    MsgLevel::Warn => theme.warn,
                    MsgLevel::Bad => theme.bad,
                };
                let p = Paragraph::new(Line::from(text.clone())).block(
                    ratatui::widgets::Block::default()
                        .borders(ratatui::widgets::Borders::ALL)
                        .border_type(ratatui::widgets::BorderType::Rounded)
                        .border_style(ratatui::style::Style::default().fg(color)),
                );
                f.render_widget(p, term_area);
            }
            _ => {}
        }

        if let Some(sf) = &mut self.settings_focus {
            draw_settings(f, area, sf, theme, self.settings_confirm_discard);
        }

        if let AppMode::Prompt {
            kind,
            buffer,
            cursor,
        } = &self.mode
        {
            self.render_prompt_overlay(f, area, theme, kind, buffer, *cursor);
        }

        if matches!(self.focus, Focus::CmdPalette) {
            self.render_palette(f, area, theme);
        }

        let hints: &[(&str, &str)] = match self.focus {
            Focus::Hosts => &[
                ("Enter", "connect"),
                ("/", "search"),
                ("n", "new"),
                ("e", "edit"),
                ("d", "del"),
                ("g", "group"),
                ("i", "import"),
                ("s", "save"),
                ("[]", "swp"),
                ("u", "update"),
                ("q", "quit"),
            ],
            Focus::Terminal => &[
                ("Ctrl+F", "search"),
                ("Ctrl+N", "new"),
                ("Ctrl+W", "close"),
                ("Ctrl+S", "sftp"),
                ("[]", "swp"),
                ("Tab", "swp"),
            ],
            Focus::Sftp => &[
                ("F5", "upload"),
                ("F6", "download"),
                ("F7", "mkdir"),
                ("F8", "delete"),
                ("r", "rename"),
                ("Esc", "shell"),
                ("[]", "swp"),
            ],
            Focus::Search => &[("Enter", "apply"), ("Esc", "cancel")],
            Focus::TermSearch => &[("Enter/Down", "next"), ("Up", "prev"), ("Esc", "exit")],
            Focus::CmdPalette => &[("Enter", "run"), ("Esc", "close")],
            Focus::Prompt => &[("Enter", "ok"), ("Esc", "cancel")],
            Focus::Settings => &[
                ("↑↓", "navigate"),
                ("Enter", "edit"),
                ("←→", "cycle"),
                ("Ctrl+S", "save"),
                ("Esc", "close"),
            ],
        };
        let capture_indicator = if self.capture_mode {
            Some("CAPTURE")
        } else {
            None
        };
        draw_status_bar(
            f,
            status_bar_area,
            theme,
            hints,
            capture_indicator.or(self.status_msg.as_deref()),
        );

        self.render_update_banner(f, area, theme);

        self.toasts.retain(|t| std::time::Instant::now() < t.until);
        draw_toasts(f, area, theme, &self.toasts);
    }

    fn render_connected(&mut self, f: &mut Frame, area: Rect, theme: &Theme, idx: usize) {
        if let Some(sid) = self.sftp_session_id {
            if Some(self.sessions[idx].id) == Some(sid) && matches!(self.mode, AppMode::Sftp) {
                let s = self.sessions[idx].sftp_state.clone();
                if let Some(s) = s {
                    self.render_sftp_fullscreen(f, area, theme, &s);
                    return;
                }
            }
        }

        let (mut lines, raw_lines, search_active, search_query, search_matches, search_current) = {
            let s = &self.sessions[idx];
            (
                s.view.render(theme),
                s.view.raw_lines(),
                s.search.active,
                s.search.query.clone(),
                s.search.matches.clone(),
                s.search.current,
            )
        };
        if search_active && !search_query.is_empty() {
            let q: Vec<char> = search_query.chars().collect();
            let mut line_matches: std::collections::BTreeMap<usize, Vec<(usize, usize)>> =
                std::collections::BTreeMap::new();
            for (mi, &(li, ci)) in search_matches.iter().enumerate() {
                line_matches.entry(li).or_default().push((mi, ci));
            }
            for (&li, matches) in &line_matches {
                if li >= raw_lines.len() || li >= lines.len() {
                    continue;
                }
                let line_str = raw_lines[li].as_str();
                let mut spans: Vec<Span> = Vec::new();
                let mut pos = 0;
                for &(mi, ci) in matches {
                    if ci < pos || ci + q.len() > line_str.len() {
                        continue;
                    }
                    if ci > pos {
                        spans.push(Span::raw(line_str[pos..ci].to_string()));
                    }
                    let is_current = mi == search_current;
                    let bg = if is_current { theme.bad } else { theme.warn };
                    let fg = if is_current { theme.bg } else { theme.txt };
                    spans.push(Span::styled(
                        line_str[ci..ci + q.len()].to_string(),
                        Style::default().bg(bg).fg(fg),
                    ));
                    pos = ci + q.len();
                }
                if pos < line_str.len() {
                    spans.push(Span::raw(line_str[pos..].to_string()));
                }
                lines[li] = Line::from(spans);
            }
        }
        let (cursor_col, cursor_row) = self.sessions[idx].view.cursor();
        let scroll = self.sessions[idx].view.scroll_offset;
        let host_title = format!(" {} ", self.sessions[idx].host_name);
        let mouse_on = self.sessions[idx].mouse_active;

        let bar_height: u16 = if matches!(self.focus, Focus::TermSearch) {
            2
        } else {
            1
        };
        let outer_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(bar_height),
            ])
            .split(area);

        let metrics_area = outer_chunks[0];
        let term_area = outer_chunks[1];
        let bar_area = outer_chunks[2];

        if !self.settings.show_metrics {
            f.render_widget(
                Paragraph::new(Line::from(Span::raw(""))).style(Style::default().bg(theme.panel)),
                metrics_area,
            );
        } else {
            let use_remote = self.remote_metrics.is_some() && self.active_session.is_some();
            let m: &RemoteMetrics = self.remote_metrics.as_ref().unwrap_or(&self.metrics);
            let up_str = format_duration(m.uptime_secs);
            let net_down = format_network_speed(m.net_down_kbs);
            let net_up = format_network_speed(m.net_up_kbs);
            let host_tag = if use_remote {
                if let Some(idx) = self.active_session {
                    format!(" {} ", self.sessions[idx].host_name)
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            let cpu_color = if m.cpu_pct > 80.0 {
                theme.cpu_high
            } else if m.cpu_pct > 50.0 {
                theme.cpu_mid
            } else {
                theme.cpu_low
            };
            let ram_pct = if m.ram_total_mb > 0 {
                m.ram_used_mb as f32 / m.ram_total_mb as f32 * 100.0
            } else {
                0.0
            };
            let ram_color = if ram_pct > 80.0 {
                theme.mem_high
            } else if ram_pct > 50.0 {
                theme.mem_mid
            } else {
                theme.mem_low
            };
            let disk_pct = if m.disk_total_gb > 0.0 {
                m.disk_used_gb / m.disk_total_gb * 100.0
            } else {
                0.0
            };
            let disk_color = if disk_pct > 80.0 {
                theme.mem_high
            } else if disk_pct > 50.0 {
                theme.mem_mid
            } else {
                theme.mem_low
            };
            let mut spans = vec![
                Span::styled(
                    format!(" CPU {:.0}% ", m.cpu_pct),
                    Style::default().fg(cpu_color).bg(theme.surface),
                ),
                Span::styled(
                    format!(" MEM {}/{}mb ", m.ram_used_mb, m.ram_total_mb),
                    Style::default().fg(ram_color).bg(theme.surface),
                ),
                Span::styled(
                    format!(" DSK {} ", format_disk(m.disk_total_gb, m.disk_used_gb)),
                    Style::default().fg(disk_color).bg(theme.surface),
                ),
                Span::styled(
                    format!(" \u{2193}{} \u{2191}{} ", net_down, net_up),
                    Style::default().fg(theme.txt).bg(theme.surface),
                ),
                Span::styled(
                    format!(" LD {:.1} ", m.load_1),
                    Style::default().fg(theme.dim).bg(theme.surface),
                ),
                Span::styled(
                    format!(" UP {} ", up_str),
                    Style::default().fg(theme.dim).bg(theme.surface),
                ),
                Span::styled(
                    format!(" {} ses ", self.sessions.len()),
                    Style::default().fg(theme.accent2).bg(theme.surface),
                ),
            ];
            if use_remote {
                spans.push(Span::styled(
                    host_tag,
                    Style::default().fg(theme.good).bg(theme.panel2),
                ));
            }
            let metrics_line = Line::from(spans);
            f.render_widget(
                Paragraph::new(metrics_line).style(Style::default().bg(theme.panel)),
                metrics_area,
            );
        }

        let show_cursor = matches!(self.focus, Focus::Terminal) && scroll == 0;
        let (cx, cy) = (cursor_col, cursor_row);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border2))
            .title(Span::styled(
                &host_title,
                Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.bg));
        let inner = block.inner(term_area);
        f.render_widget(block, term_area);

        let para = Paragraph::new(lines);
        f.render_widget(para, inner);

        if show_cursor && cx < inner.width && cy < inner.height {
            f.set_cursor_position((inner.x + cx, inner.y + cy));
        }

        let (tab_area, search_bar_area) = if matches!(self.focus, Focus::TermSearch) {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(1)])
                .split(bar_area);
            (chunks[0], Some(chunks[1]))
        } else {
            (bar_area, None)
        };

        if let Some(sba) = search_bar_area {
            let bar_extra = if idx < self.sessions.len() {
                let total = self.sessions[idx].search.matches.len();
                let cur = if total > 0 {
                    self.sessions[idx].search.current + 1
                } else {
                    0
                };
                format!(" {}/{} ", cur, total)
            } else {
                String::new()
            };
            let search_text = format!("/ {}{}", self.sessions[idx].search.query, bar_extra);
            let search_span =
                Span::styled(search_text, Style::default().fg(theme.txt).bg(theme.accent));
            f.render_widget(
                Paragraph::new(Line::from(search_span)).style(Style::default().bg(theme.panel)),
                sba,
            );
        }

        let tab_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(15),
                Constraint::Min(10),
                Constraint::Min(40),
            ])
            .split(tab_area);

        let mut tabs_spans: Vec<Span> = Vec::new();
        for (i, s) in self.sessions.iter().enumerate() {
            let is_active = i == idx;
            let (icon, status_fg) = match &s.status {
                SessionStatus::Connecting => ("\u{25b6}", theme.warn),
                SessionStatus::Active => ("\u{25cf}", theme.good),
                SessionStatus::Disconnected(_) => ("\u{2716}", theme.bad),
            };
            let bg = if is_active {
                theme.accent2
            } else {
                theme.surface
            };
            let fg = if is_active { theme.bg } else { status_fg };
            let label = format!(" #{}.{} {} ", i + 1, icon, truncate(&s.host_name, 12));
            tabs_spans.push(Span::styled(label, Style::default().fg(fg).bg(bg)));
            tabs_spans.push(Span::raw(" "));
        }
        if !tabs_spans.is_empty() {
            f.render_widget(
                Paragraph::new(Line::from(tabs_spans)).style(Style::default().bg(theme.panel)),
                tab_chunks[0],
            );
        } else {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " (no sessions)",
                    Style::default().fg(theme.dim),
                ))),
                tab_chunks[0],
            );
        }

        let title_line = Line::from(Span::styled(&host_title, Style::default().fg(theme.dim)));
        f.render_widget(
            Paragraph::new(title_line).style(Style::default().bg(theme.panel)),
            tab_chunks[1],
        );

        let ioff = if mouse_on { "·on" } else { "·off" };
        let mouse_str = format!("M·{} ", ioff);
        let hints: Vec<Span> = vec![
            Span::styled("Q", Style::default().fg(theme.bg).bg(theme.bad)),
            Span::styled(" exit ", Style::default().fg(theme.txt)),
            Span::styled("W", Style::default().fg(theme.bg).bg(theme.bad)),
            Span::styled(" close ", Style::default().fg(theme.txt)),
            Span::styled("S", Style::default().fg(theme.bg).bg(theme.accent)),
            Span::styled(" sftp ", Style::default().fg(theme.txt)),
            Span::styled(
                mouse_str.as_str(),
                Style::default()
                    .fg(theme.txt)
                    .bg(if mouse_on { theme.bad } else { theme.panel2 }),
            ),
            Span::styled("\\", Style::default().fg(theme.bg).bg(theme.warn)),
            Span::styled(" cap ", Style::default().fg(theme.txt)),
            Span::styled("Tab", Style::default().fg(theme.bg).bg(theme.accent)),
            Span::styled(" next ", Style::default().fg(theme.txt)),
        ];
        f.render_widget(
            Paragraph::new(Line::from(hints)).style(Style::default().bg(theme.panel)),
            tab_chunks[2],
        );

        self.toasts.retain(|t| std::time::Instant::now() < t.until);
        draw_toasts(f, area, theme, &self.toasts);
    }

    fn render_sftp_fullscreen(&mut self, f: &mut Frame, area: Rect, theme: &Theme, s: &SftpState) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);

        draw_sftp(f, chunks[0], theme, s);

        let hints = [
            ("Tab", "switch"),
            ("F5", "upload"),
            ("F6", "download"),
            ("F7", "mkdir"),
            ("F8", "delete"),
            ("r", "rename"),
            ("/", "filter"),
            ("Esc", "back"),
        ];
        let cap = if self.capture_mode {
            Some("CAPTURE")
        } else {
            None
        };
        draw_status_bar(
            f,
            chunks[1],
            theme,
            &hints,
            cap.or(self.status_msg.as_deref()),
        );

        self.toasts.retain(|t| std::time::Instant::now() < t.until);
        draw_toasts(f, area, theme, &self.toasts);
    }

    fn render_prompt_overlay(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        kind: &PromptKind,
        buffer: &str,
        cursor: usize,
    ) {
        let label = prompt_label(kind, buffer);
        let hidden = matches!(
            kind,
            PromptKind::Password { .. }
                | PromptKind::Passphrase { .. }
                | PromptKind::JumpPassword { .. }
        );
        let display: String = if hidden {
            "*".repeat(buffer.chars().count())
        } else {
            buffer.to_string()
        };
        let area = popup_area(area, 60, 20);
        draw_prompt(f, area, theme, &label, &display, cursor);
    }

    fn render_palette(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let items = self.palette_items();
        let filtered: Vec<&(String, String)> = items
            .iter()
            .filter(|(label, _)| {
                self.palette_filter.is_empty()
                    || label
                        .to_lowercase()
                        .contains(&self.palette_filter.to_lowercase())
            })
            .collect();

        let h = (filtered.len() as u16 + 4).min(20);
        let w = 50.min(area.width.saturating_sub(4));
        let popup = Rect {
            x: (area.width.saturating_sub(w)) / 2,
            y: area.y.saturating_add(1),
            width: w,
            height: h,
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.accent))
            .title(Span::styled(
                " Command Palette ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.panel));
        let inner = block.inner(popup);
        f.render_widget(Clear, popup);
        f.render_widget(block, popup);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(inner);

        let filter_text = format!("> {}", self.palette_filter);
        let filter_para = Paragraph::new(Line::from(Span::styled(
            filter_text,
            Style::default().fg(theme.txt),
        )))
        .style(Style::default().bg(theme.surface));
        f.render_widget(filter_para, chunks[0]);

        let mut list_items: Vec<ListItem> = Vec::new();
        for (i, (label, key)) in filtered.iter().enumerate() {
            let selected = i == self.palette_selected.min(filtered.len().saturating_sub(1));
            let style = if selected {
                Style::default().bg(theme.sel_bg).fg(theme.sel_fg)
            } else {
                Style::default().fg(theme.txt)
            };
            let line = Line::from(vec![
                Span::styled(format!(" {} ", label), style),
                Span::styled(format!("[{}]", key), Style::default().fg(theme.dim)),
            ]);
            list_items.push(ListItem::new(line));
        }
        let list = List::new(list_items).style(Style::default().bg(theme.panel));
        f.render_widget(list, chunks[1]);
    }

    fn render_details(&self, theme: &Theme) -> Vec<Line<'static>> {
        if self.hosts.is_empty() {
            return vec![Line::from(Span::styled(
                " No hosts configured. Press n to add.",
                Style::default().fg(theme.dim),
            ))];
        }
        let Some(h) = self.selected_host() else {
            return vec![Line::from(Span::styled(
                " No host selected.",
                Style::default().fg(theme.dim),
            ))];
        };
        let auth_str = h
            .identity
            .iter()
            .map(|i| match i {
                Identity::Key { path, .. } => format!("key:{}", path),
                Identity::Password { .. } => "password".into(),
                Identity::Agent => "agent".into(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let name = h.name.clone();
        let addr = h.addr();
        let user = h.user.clone();
        let group = h.group.clone().unwrap_or_else(|| "-".to_string());
        let tags = if h.tags.is_empty() {
            "-".to_string()
        } else {
            h.tags.join(", ")
        };
        let jump = h.jump.clone().unwrap_or_else(|| "-".to_string());
        let keepalive = format!("{}s", h.keepalive.unwrap_or(0));
        let address = format!("{}@{}", user, addr);
        vec![
            Line::from(vec![
                Span::styled(" Name     ", Style::default().fg(theme.dim)),
                Span::styled(
                    name,
                    Style::default().fg(theme.txt).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Address  ", Style::default().fg(theme.dim)),
                Span::styled(address, Style::default().fg(theme.txt)),
            ]),
            Line::from(vec![
                Span::styled(" Group    ", Style::default().fg(theme.dim)),
                Span::styled(group, Style::default().fg(theme.txt)),
            ]),
            Line::from(vec![
                Span::styled(" Tags     ", Style::default().fg(theme.dim)),
                Span::styled(tags, Style::default().fg(theme.txt)),
            ]),
            Line::from(vec![
                Span::styled(" Auth     ", Style::default().fg(theme.dim)),
                Span::styled(
                    if auth_str.is_empty() {
                        "-".into()
                    } else {
                        auth_str
                    },
                    Style::default().fg(theme.txt),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Jump     ", Style::default().fg(theme.dim)),
                Span::styled(jump, Style::default().fg(theme.txt)),
            ]),
            Line::from(vec![
                Span::styled(" Keepalive", Style::default().fg(theme.dim)),
                Span::styled(keepalive, Style::default().fg(theme.txt)),
            ]),
            Line::from(vec![
                Span::styled(" On conn  ", Style::default().fg(theme.dim)),
                Span::styled(
                    if h.on_connect.is_empty() {
                        "-".into()
                    } else {
                        h.on_connect.join("; ")
                    },
                    Style::default().fg(theme.txt),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(" Enter ", Style::default().fg(theme.bg).bg(theme.accent2)),
                Span::raw(" connect  "),
                Span::styled(" e ", Style::default().fg(theme.bg).bg(theme.accent2)),
                Span::raw(" edit  "),
                Span::styled(" d ", Style::default().fg(theme.bg).bg(theme.accent2)),
                Span::raw(" delete  "),
                Span::styled(" s ", Style::default().fg(theme.bg).bg(theme.accent2)),
                Span::raw(" save"),
            ]),
        ]
    }

    fn render_update_banner(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        if self.update_dismissed {
            return;
        }
        let (title, lines, is_err) = match &self.update_status {
            UpdateStatus::Available => {
                let ver = &self.update_latest_version;
                let cur = update::current_version();
                (
                    " Update Available ",
                    vec![
                        Line::from(vec![
                            Span::raw("  "),
                            Span::styled("\u{2726}", Style::default().fg(theme.accent)),
                            Span::raw(format!(" v{} ready  ", ver)),
                            Span::styled(
                                format!("(you have v{})", cur),
                                Style::default().fg(theme.dim),
                            ),
                        ]),
                        Line::from(vec![
                            Span::raw("  "),
                            Span::styled(" u ", Style::default().fg(theme.sel_bg).bg(theme.sel_fg)),
                            Span::raw(" install  "),
                            Span::styled(
                                " Esc ",
                                Style::default().fg(theme.sel_bg).bg(theme.sel_fg),
                            ),
                            Span::raw(" dismiss"),
                        ]),
                    ],
                    false,
                )
            }
            UpdateStatus::Downloading => (
                " Downloading Update ",
                vec![Line::from(Span::styled(
                    "  \u{25cf} downloading...",
                    Style::default().fg(theme.warn),
                ))],
                false,
            ),
            UpdateStatus::Done => (
                " Update Installed ",
                vec![Line::from(vec![
                    Span::styled("  \u{2713} ", Style::default().fg(theme.good)),
                    Span::styled(
                        format!("v{}", self.update_latest_version),
                        Style::default().fg(theme.accent),
                    ),
                    Span::raw(" installed! Restart betterssh to use."),
                ])],
                false,
            ),
            UpdateStatus::Failed(e) => (
                " Update Failed ",
                vec![Line::from(Span::styled(
                    format!("  \u{2717} {}", e),
                    Style::default().fg(theme.bad),
                ))],
                true,
            ),
            _ => return,
        };

        let h = lines.len() as u16 + 2;
        let w = area.width.min(58);
        let popup = Rect {
            x: (area.width - w) / 2,
            y: 0,
            width: w,
            height: h,
        };
        let border_c = if is_err { theme.bad } else { theme.accent };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_c))
            .title(Span::styled(
                title,
                Style::default().fg(border_c).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.surface));
        let inner = block.inner(popup);
        f.render_widget(Clear, popup);
        f.render_widget(block, popup);
        for (i, line) in lines.iter().enumerate() {
            f.render_widget(
                Paragraph::new(line.clone()).style(Style::default().bg(theme.surface)),
                Rect {
                    x: inner.x,
                    y: inner.y + i as u16,
                    width: inner.width,
                    height: 1,
                },
            );
        }
    }

    fn handle_event(
        &mut self,
        evt: Event,
        cmd_tx: &mpsc::UnboundedSender<Cmd>,
        settings: &Arc<Settings>,
    ) {
        match evt {
            Event::Key(k) => {
                if k.kind == KeyEventKind::Press || k.kind == KeyEventKind::Repeat {
                    self.handle_key(k, cmd_tx, settings);
                }
            }
            Event::Mouse(m) => {
                if !matches!(self.focus, Focus::Terminal) && !self.capture_mode {
                    return;
                }
                let active_idx = self.active_session;

                match m.kind {
                    crossterm::event::MouseEventKind::ScrollUp => {
                        if let Some(idx) = active_idx {
                            if let Some(s) = self.sessions.get_mut(idx) {
                                s.view.scroll_up(3);
                            }
                        }
                        return;
                    }
                    crossterm::event::MouseEventKind::ScrollDown => {
                        if let Some(idx) = active_idx {
                            if let Some(s) = self.sessions.get_mut(idx) {
                                s.view.scroll_down(3);
                            }
                        }
                        return;
                    }
                    _ => {}
                }

                let mouse_active = active_idx
                    .and_then(|idx| self.sessions.get(idx))
                    .map(|s| s.mouse_active)
                    .unwrap_or(false);
                if !mouse_active {
                    return;
                }

                if let Some(idx) = active_idx {
                    let term_rows = self.sessions[idx].tx_rows;
                    let term_cols = self.sessions[idx].tx_cols;
                    if let Some(tx) = self.sessions[idx].cmd_tx.as_ref() {
                        match m.kind {
                            crossterm::event::MouseEventKind::Moved
                            | crossterm::event::MouseEventKind::ScrollLeft
                            | crossterm::event::MouseEventKind::ScrollRight => return,
                            _ => {}
                        }
                        let sn_col = m.column.max(1);
                        let sn_row = m.row.max(1);
                        let col = sn_col.clamp(1, term_cols);
                        let row = sn_row.clamp(1, term_rows);
                        let bytes = match m.kind {
                            crossterm::event::MouseEventKind::Down(btn) => {
                                let b = match btn {
                                    crossterm::event::MouseButton::Left => 0,
                                    crossterm::event::MouseButton::Middle => 1,
                                    crossterm::event::MouseButton::Right => 2,
                                };
                                format!("\x1b[<{};{};{}M", b, col, row).into_bytes()
                            }
                            crossterm::event::MouseEventKind::Up(btn) => {
                                let b = match btn {
                                    crossterm::event::MouseButton::Left => 0,
                                    crossterm::event::MouseButton::Middle => 1,
                                    crossterm::event::MouseButton::Right => 2,
                                };
                                format!("\x1b[<{};{};{}m", b, col, row).into_bytes()
                            }
                            crossterm::event::MouseEventKind::Drag(_) => vec![],
                            _ => vec![],
                        };
                        if !bytes.is_empty() {
                            let _ = tx.send(SessionCmd::Send(bytes));
                        }
                    }
                }
            }
            Event::Resize(cols, rows) => {
                let active_idx = self.active_session;
                let bar_height = if matches!(self.focus, Focus::TermSearch) {
                    2u16
                } else {
                    1u16
                };
                let (term_cols, term_rows) = if active_idx.is_some() {
                    (
                        cols.saturating_sub(2).max(20),
                        rows.saturating_sub(1 + bar_height + 2).max(5),
                    )
                } else {
                    (
                        cols.saturating_sub(34).max(20),
                        rows.saturating_sub(1).max(5),
                    )
                };
                self.term_cols = term_cols;
                self.term_rows = term_rows;

                self.pending_resize = Some((term_cols, term_rows));
            }
            _ => {}
        }
    }

    fn handle_key(
        &mut self,
        k: KeyEvent,
        cmd_tx: &mpsc::UnboundedSender<Cmd>,
        settings: &Arc<Settings>,
    ) {
        let key_str = key_event_to_string(&k);
        if !key_str.is_empty() {
            if let Some(action) = self
                .settings
                .keybindings
                .iter()
                .find(|(_, v)| v == &&key_str)
                .map(|(k, _)| k.clone())
            {
                match action.as_str() {
                    "command_palette" => {
                        if !matches!(self.focus, Focus::CmdPalette) {
                            self.palette_filter.clear();
                            self.palette_selected = 0;
                            self.focus = Focus::CmdPalette;
                            return;
                        }
                    }
                    "quit" => {
                        self.should_quit = true;
                        return;
                    }
                    "save_config" => {
                        self.save_config(settings);
                        return;
                    }
                    "toggle_group" => {
                        self.group_mode = !self.group_mode;
                        return;
                    }
                    "import_ssh" => {
                        let _ = self.import_ssh_config();
                        return;
                    }
                    "new_host" => {
                        self.mode = AppMode::Prompt {
                            kind: PromptKind::NewHost,
                            buffer: String::new(),
                            cursor: 0,
                        };
                        return;
                    }
                    "open_settings" => {
                        self.open_settings();
                        return;
                    }
                    "update" => {
                        update::do_install();
                        return;
                    }
                    _ => {}
                }
            }
        }

        if matches!(self.update_status, UpdateStatus::Available) && !self.update_dismissed {
            match k.code {
                KeyCode::Char('u') | KeyCode::Char('U') => {
                    update::do_install();
                    return;
                }
                KeyCode::Esc => {
                    self.update_dismissed = true;
                    self.update_status = UpdateStatus::Idle;
                    return;
                }
                _ => {}
            }
        }
        if matches!(
            self.update_status,
            UpdateStatus::Done | UpdateStatus::Failed(_)
        ) && k.code == KeyCode::Esc
        {
            self.update_status = UpdateStatus::Idle;
            return;
        }

        if !matches!(self.focus, Focus::CmdPalette)
            && k.modifiers.contains(KeyModifiers::CONTROL)
            && k.code == KeyCode::Char('p')
        {
            self.palette_filter.clear();
            self.palette_selected = 0;
            self.focus = Focus::CmdPalette;
            return;
        }

        if k.modifiers.contains(KeyModifiers::CONTROL)
            && !k.modifiers.contains(KeyModifiers::ALT)
            && self.active_session.is_some()
            && (k.code == KeyCode::Char('b')
                || k.code == KeyCode::Char('B')
                || k.code == KeyCode::Char('\\'))
        {
            self.capture_mode = !self.capture_mode;
            self.focus = Focus::Terminal;
            if self.capture_mode {
                self.push_toast("CAPTURE ON (Ctrl+B to exit)".to_string(), MsgLevel::Info);
            } else {
                self.push_toast("capture off".to_string(), MsgLevel::Info);
            }
            return;
        }

        if k.modifiers.contains(KeyModifiers::ALT) && k.code == KeyCode::Char('m') {
            if let Some(idx) = self.active_session {
                if idx < self.sessions.len() {
                    let active = !self.sessions[idx].mouse_active;
                    self.sessions[idx].mouse_active = active;
                    if active {
                        if let Some(tx) = self.sessions[idx].cmd_tx.as_ref() {
                            let _ = tx.send(SessionCmd::Send(b"\x1b[?1003h\x1b[?1006h".to_vec()));
                        }
                        self.push_toast("mouse ON".to_string(), MsgLevel::Info);
                    } else {
                        if let Some(tx) = self.sessions[idx].cmd_tx.as_ref() {
                            let _ = tx.send(SessionCmd::Send(b"\x1b[?1003l\x1b[?1006l".to_vec()));
                        }
                        self.push_toast("mouse OFF".to_string(), MsgLevel::Info);
                    }
                    self.sync_mouse_capture();
                }
            }
            return;
        }

        if matches!(self.mode, AppMode::Sftp) {
            self.handle_sftp_key(k, cmd_tx);
            return;
        }

        if let Some(idx) = self.active_session {
            if (k.code == KeyCode::Tab || k.code == KeyCode::BackTab)
                && !k.modifiers.contains(KeyModifiers::CONTROL)
                && !self.capture_mode
            {
                let dir = if k.code == KeyCode::BackTab { -1 } else { 1 };
                let prev = self.active_session;
                self.cycle_session(dir);

                self.focus = Focus::Terminal;
                if self.active_session != prev {
                    self.sync_mouse_capture();
                    self.push_toast(
                        format!("session {}", self.active_session.unwrap() + 1),
                        MsgLevel::Info,
                    );
                }
                return;
            }

            if !self.capture_mode
                && k.modifiers.contains(KeyModifiers::CONTROL)
                && k.code == KeyCode::Char('q')
            {
                if self.sessions.len() > 1 {
                    self.close_session_by_index_sync(idx);
                } else {
                    self.should_quit = true;
                }
                return;
            }

            if !self.capture_mode
                && k.modifiers.contains(KeyModifiers::CONTROL)
                && (k.code == KeyCode::Char('w') || k.code == KeyCode::Char('W'))
            {
                if self.sessions.is_empty() {
                    self.should_quit = true;
                } else {
                    self.close_session_by_index_sync(idx);
                }
                return;
            }

            if !self.capture_mode
                && k.modifiers.contains(KeyModifiers::CONTROL)
                && k.code == KeyCode::Char('t')
            {
                self.active_session = None;
                self.focus = Focus::Hosts;
                return;
            }

            if !self.capture_mode
                && k.modifiers.contains(KeyModifiers::CONTROL)
                && k.code == KeyCode::Char('n')
            {
                self.active_session = None;
                self.focus = Focus::Hosts;
                return;
            }

            if matches!(self.focus, Focus::CmdPalette) {
                self.handle_palette_key(k, cmd_tx, settings);
                return;
            }

            if !self.capture_mode
                && k.modifiers.contains(KeyModifiers::CONTROL)
                && k.code == KeyCode::Char('p')
            {
                self.palette_filter.clear();
                self.palette_selected = 0;
                self.focus = Focus::CmdPalette;
                return;
            }

            if k.modifiers.is_empty()
                && (k.code == KeyCode::Char('[') || k.code == KeyCode::Char(']'))
                && matches!(self.focus, Focus::Hosts | Focus::Search)
            {
                let dir = if k.code == KeyCode::Char('[') { -1 } else { 1 };
                let prev = self.active_session;
                self.cycle_session(dir);
                if self.active_session != prev {
                    self.focus = Focus::Terminal;
                    self.sync_mouse_capture();
                    self.push_toast(
                        format!("session {}", self.active_session.unwrap() + 1),
                        MsgLevel::Info,
                    );
                }
                return;
            }

            if matches!(self.focus, Focus::Hosts) {
                self.handle_hosts_key(k, cmd_tx, settings);
                return;
            }

            if !self.capture_mode
                && k.modifiers.contains(KeyModifiers::CONTROL)
                && k.code == KeyCode::Char('s')
            {
                if idx < self.sessions.len() {
                    self.open_sftp_for_session(idx);
                }
                return;
            }

            if !self.capture_mode
                && k.modifiers.is_empty()
                && matches!(k.code, KeyCode::Char(c) if c.is_ascii_digit() && c != '0')
            {
                if let KeyCode::Char(c) = k.code {
                    let idx_s = (c as u8 - b'1') as usize;
                    if let Some(snip) = self.snippets.get(idx_s).cloned() {
                        if let Some(sess) = self.sessions.get_mut(idx) {
                            if let Some(tx) = sess.cmd_tx.as_ref() {
                                let mut bytes = snip.cmd.into_bytes();
                                bytes.push(b'\n');
                                let _ = tx.send(SessionCmd::Send(bytes));
                                return;
                            }
                        }
                    }
                }
            }

            if k.code == KeyCode::PageUp {
                if let Some(sess) = self.sessions.get_mut(idx) {
                    sess.view.scroll_up(5);
                }
                return;
            }
            if k.code == KeyCode::PageDown {
                if let Some(sess) = self.sessions.get_mut(idx) {
                    sess.view.scroll_down(5);
                }
                return;
            }

            if k.code == KeyCode::F(2) && !self.capture_mode {
                if let Some(s) = self.sessions.get(idx) {
                    let current = s.label.clone();
                    let cur_len = current.len();
                    self.mode = AppMode::Prompt {
                        kind: PromptKind::RenameSession { session_idx: idx },
                        buffer: current,
                        cursor: cur_len,
                    };
                }
                return;
            }

            if !self.capture_mode
                && k.modifiers.contains(KeyModifiers::CONTROL)
                && k.code == KeyCode::Char('f')
            {
                if let Some(sess) = self.sessions.get_mut(idx) {
                    sess.search.active = true;
                    self.focus = Focus::TermSearch;
                }
                return;
            }

            if self.capture_mode {
                if let Some(sess) = self.sessions.get_mut(idx) {
                    let bytes = key_to_bytes(k);
                    if !bytes.is_empty() {
                        if let Some(tx) = sess.cmd_tx.as_ref() {
                            let _ = tx.send(SessionCmd::Send(bytes));
                        }
                    }
                }
                return;
            }

            if let Some(sess) = self.sessions.get_mut(idx) {
                let bytes = key_to_bytes(k);
                if bytes.is_empty() {
                    tracing::debug!(?k, "key_to_bytes returned empty");
                } else {
                    tracing::debug!(?k, bytes = %bytes.iter().map(|b| format!("\\x{:02x}", b)).collect::<Vec<_>>().join(""), "sending to ssh");
                    if let Some(tx) = sess.cmd_tx.as_ref() {
                        let _ = tx.send(SessionCmd::Send(bytes));
                    }
                }
            }
            return;
        }

        if !self.sessions.is_empty()
            && !matches!(self.mode, AppMode::Prompt { .. })
            && k.code == KeyCode::Tab
            && !k.modifiers.contains(KeyModifiers::CONTROL)
            && !self.capture_mode
        {
            self.focus = Focus::Terminal;

            let first = self
                .sessions
                .iter()
                .position(|s| s.is_active() || s.disconnected().is_some())
                .or(self.active_session);
            if let Some(i) = first {
                self.active_session = Some(i);
                return;
            }
        }

        if matches!(self.mode, AppMode::Prompt { .. }) {
            self.handle_prompt_key(k, settings);
            return;
        }

        if matches!(self.mode, AppMode::Sftp) {
            self.handle_sftp_key(k, cmd_tx);
            return;
        }
        if matches!(self.focus, Focus::Search) {
            self.handle_search_key(k, cmd_tx, settings);
            return;
        }
        if matches!(self.focus, Focus::TermSearch) {
            self.handle_term_search_key(k, cmd_tx, settings);
            return;
        }
        if matches!(self.focus, Focus::CmdPalette) {
            self.handle_palette_key(k, cmd_tx, settings);
            return;
        }
        match self.focus {
            Focus::Hosts => self.handle_hosts_key(k, cmd_tx, settings),
            Focus::Terminal => self.handle_terminal_key(k, cmd_tx, settings),
            Focus::Sftp => {}
            Focus::Prompt => {}
            Focus::Search => {}
            Focus::TermSearch => {}
            Focus::CmdPalette => {}
            Focus::Settings => self.handle_settings_key(k),
        }
    }

    fn handle_settings_key(&mut self, k: KeyEvent) {
        let Some(sf) = &mut self.settings_focus else {
            self.focus = Focus::Hosts;
            return;
        };
        if self.settings_confirm_discard {
            match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.settings_focus = None;
                    self.settings_confirm_discard = false;
                    self.focus = Focus::Hosts;
                }
                _ => {
                    self.settings_confirm_discard = false;
                }
            }
            return;
        }
        match sf.handle_key(k) {
            Some(SettingsAction::Save) => {
                sf.apply(&mut self.settings);
                self.settings_focus = None;
                self.settings_confirm_discard = false;
                self.focus = Focus::Hosts;
                self.save_config_to_disk();
                self.reload_theme();
                self.status_msg = Some("settings saved".into());
            }
            Some(SettingsAction::Close) => {
                self.settings_focus = None;
                self.settings_confirm_discard = false;
                self.focus = Focus::Hosts;
            }
            Some(SettingsAction::ConfirmDiscard) => {
                self.settings_confirm_discard = true;
            }
            Some(SettingsAction::EditKeybinding { idx }) => {
                let entries: Vec<(String, String)> = self
                    .settings
                    .keybindings
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if let Some((action, current)) = entries.get(idx) {
                    self.mode = AppMode::Prompt {
                        kind: PromptKind::KeybindingEdit {
                            action: action.clone(),
                            current: current.clone(),
                        },
                        buffer: current.clone(),
                        cursor: current.len(),
                    };
                }
            }
            Some(SettingsAction::AddKeybinding) => {
                self.mode = AppMode::Prompt {
                    kind: PromptKind::KeybindingNew,
                    buffer: String::new(),
                    cursor: 0,
                };
            }
            Some(SettingsAction::EditMacro { idx }) => {
                if let Some(m) = self.settings.macros.get(idx) {
                    self.mode = AppMode::Prompt {
                        kind: PromptKind::MacroName {
                            current: m.name.clone(),
                        },
                        buffer: m.name.clone(),
                        cursor: m.name.len(),
                    };

                    self.edit_target = Some(format!("macro_{}", idx));
                }
            }
            Some(SettingsAction::AddMacro) => {
                self.edit_target = None;
                self.mode = AppMode::Prompt {
                    kind: PromptKind::MacroName {
                        current: String::new(),
                    },
                    buffer: String::new(),
                    cursor: 0,
                };
            }
            None => {}
        }
    }

    fn import_ssh_config(&mut self) -> Result<()> {
        let path = ssh_config_path();
        if !path.exists() {
            self.status_msg = Some(format!("not found: {}", path.display()));
            return Ok(());
        }
        let entries = parse_ssh_config(&path)?;
        if entries.is_empty() {
            self.status_msg = Some("no hosts found in SSH config".into());
            return Ok(());
        }
        let imported = entries_to_hosts(&entries);
        let before = self.hosts.len();
        self.hosts = merge_hosts(&self.hosts, imported);
        let added = self.hosts.len() - before;
        self.status_msg = Some(format!("imported {} hosts from ~/.ssh/config", added));
        self.save_config(&self.settings);

        self.host_state.select(Some(0));
        Ok(())
    }

    fn save_config_to_disk(&self) {
        self.save_config(&self.settings);
    }

    fn reload_theme(&mut self) {
        self.theme = theme::load_theme(&self.settings.theme);
    }

    fn close_session_by_index_sync(&mut self, idx: usize) {
        if idx >= self.sessions.len() {
            return;
        }
        if let Some(tx) = self.sessions[idx].cmd_tx.as_ref() {
            let _ = tx.send(SessionCmd::Send(b"\x1b[?1003l\x1b[?1006l".to_vec()));
            let _ = tx.send(SessionCmd::Stop);
        }
        if let Some(sid) = self.sftp_session_id {
            if Some(self.sessions[idx].id) == Some(sid) {
                self.sftp_session_id = None;
                if matches!(self.mode, AppMode::Sftp) {
                    self.mode = AppMode::Browsing;
                }
            }
        }
        self.sessions.remove(idx);
        if self.sessions.is_empty() {
            self.active_session = None;
            self.focus = Focus::Hosts;
            let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
        } else {
            let new_idx = if idx >= self.sessions.len() {
                self.sessions.len() - 1
            } else {
                idx
            };
            self.active_session = Some(new_idx);
            self.focus = Focus::Terminal;
            self.sync_mouse_capture();
        }
    }

    fn sync_mouse_capture(&self) {
        let on = self.mouse_active_now();
        if on {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
        } else {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
        }
    }

    fn handle_sftp_key(&mut self, k: KeyEvent, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        let sid = match self.sftp_session_id {
            Some(id) => id,
            None => return,
        };
        let idx = match self.session_index(sid) {
            Some(i) => i,
            None => {
                self.sftp_session_id = None;
                if matches!(self.mode, AppMode::Sftp) {
                    self.mode = AppMode::Browsing;
                }
                return;
            }
        };
        let sftp = match self.sessions[idx].sftp_state.as_mut() {
            Some(s) => s,
            None => return,
        };

        match (k.code, k.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => {
                self.sessions[idx].sftp_state = None;
                self.sftp_session_id = None;
                if matches!(self.mode, AppMode::Sftp) {
                    self.mode = AppMode::Browsing;
                }

                if self.active_session.is_none() {
                    self.active_session = Some(idx);
                }
                self.focus = Focus::Terminal;
                self.push_toast("sftp closed", MsgLevel::Info);
            }
            (KeyCode::Tab, _) => {
                sftp.focus = match sftp.focus {
                    SftpPane::Local => SftpPane::Remote,
                    SftpPane::Remote => SftpPane::Local,
                };
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => sftp.move_sel(-1),
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => sftp.move_sel(1),
            (KeyCode::PageUp, _) => sftp.move_sel(-10),
            (KeyCode::PageDown, _) => sftp.move_sel(10),
            (KeyCode::Home, _) => sftp.sel = 0,
            (KeyCode::End, _) => {
                let n = sftp.current_entries().len();
                if n > 0 {
                    sftp.sel = n - 1;
                }
            }
            (KeyCode::Char('/'), _) => {
                let id = self.sessions[idx].id;
                self.mode = AppMode::Prompt {
                    kind: PromptKind::SftpFilter { session_id: id },
                    buffer: String::new(),
                    cursor: 0,
                };
            }
            (KeyCode::Enter, _) | (KeyCode::Right, _) | (KeyCode::Char('l'), _) => {
                let _ = cmd_tx.send(Cmd::SftpEnter);
            }
            (KeyCode::Backspace, _) | (KeyCode::Left, _) | (KeyCode::Char('h'), _) => {
                let _ = cmd_tx.send(Cmd::SftpUpDir);
            }
            (KeyCode::F(5), _) => {
                let _ = cmd_tx.send(Cmd::SftpUpload);
            }
            (KeyCode::F(6), _) => {
                let _ = cmd_tx.send(Cmd::SftpDownload);
            }
            (KeyCode::F(7), _) => {
                let id = self.sessions[idx].id;
                self.mode = AppMode::Prompt {
                    kind: PromptKind::SftpMkdir { session_id: id },
                    buffer: String::new(),
                    cursor: 0,
                };
            }
            (KeyCode::F(8), _) => {
                let _ = cmd_tx.send(Cmd::SftpDelete);
            }
            (KeyCode::Char('r'), _) => {
                let id = self.sessions[idx].id;
                self.mode = AppMode::Prompt {
                    kind: PromptKind::SftpRename { session_id: id },
                    buffer: String::new(),
                    cursor: 0,
                };
            }
            _ => {}
        }
    }

    fn handle_hosts_key(
        &mut self,
        k: KeyEvent,
        cmd_tx: &mpsc::UnboundedSender<Cmd>,
        settings: &Arc<Settings>,
    ) {
        match (k.code, k.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => {
                self.should_quit = true;
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            (KeyCode::Char('s'), KeyModifiers::CONTROL) | (KeyCode::Char('s'), _) => {
                self.save_config(settings);
                self.status_msg = Some(format!(
                    "saved to {}",
                    betterssh_core::config_path().unwrap().display()
                ));
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => self.move_selection(-1),
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => self.move_selection(1),
            (KeyCode::PageUp, _) => self.move_selection(-10),
            (KeyCode::PageDown, _) => self.move_selection(10),
            (KeyCode::Home, _) => self.host_state.select(Some(0)),
            (KeyCode::End, _) => {
                let n = self.filtered_indices().len();
                if n > 0 {
                    self.host_state.select(Some(n - 1));
                }
            }
            (KeyCode::Char('/'), _) => {
                self.filter.clear();
                self.focus = Focus::Search;
            }
            (KeyCode::Char('n'), _) => {
                self.mode = AppMode::Prompt {
                    kind: PromptKind::NewHost,
                    buffer: String::new(),
                    cursor: 0,
                };
            }
            (KeyCode::Char('e'), _) => {
                if let Some(h) = self.selected_host().cloned() {
                    self.edit_target = Some(h.name.clone());
                    let field = EditField::Name;
                    let original = h.name.clone();
                    self.mode = AppMode::Prompt {
                        kind: PromptKind::EditField {
                            host_name: h.name,
                            field,
                            original: original.clone(),
                        },
                        buffer: original.clone(),
                        cursor: original.len(),
                    };
                }
            }
            (KeyCode::Char('p'), _) => {
                if let Some(h) = self.selected_host().cloned() {
                    let has = h
                        .identity
                        .iter()
                        .any(|i| matches!(i, Identity::Password { .. }));
                    let initial = if has { "y".into() } else { String::new() };
                    let cur = initial.len();
                    self.mode = AppMode::Prompt {
                        kind: PromptKind::EditField {
                            host_name: h.name,
                            field: EditField::Password,
                            original: initial.clone(),
                        },
                        buffer: initial,
                        cursor: cur,
                    };
                }
            }
            (KeyCode::Char('d'), _) => {
                if let Some(h) = self.selected_host() {
                    let name = h.name.clone();
                    self.mode = AppMode::Prompt {
                        kind: PromptKind::DeleteConfirm { host: name },
                        buffer: String::new(),
                        cursor: 0,
                    };
                }
            }
            (KeyCode::Char('i'), _) => {
                let _ = self.import_ssh_config();
            }
            (KeyCode::Tab, _) => {
                self.focus = Focus::Terminal;
            }
            (KeyCode::Enter, _) => {
                if let Some(h) = self.selected_host().cloned() {
                    let _ = cmd_tx.send(Cmd::Connect { host: Box::new(h) });
                }
            }
            (KeyCode::F(2), _) => {
                self.open_settings();
            }
            (KeyCode::Char('g'), _) => {
                self.group_mode = !self.group_mode;
                self.status_msg = Some(if self.group_mode {
                    "group view on".into()
                } else {
                    "group view off".into()
                });
                self.host_state.select(Some(0));

                if self.group_mode
                    && self.selected_host().is_none()
                    && self.host_state.selected() == Some(0)
                {
                    self.move_selection(1);
                }
            }
            _ => {}
        }
    }

    fn open_settings(&mut self) {
        self.settings_focus = Some(SettingsFocus::new(&self.settings));
        self.settings_confirm_discard = false;
        self.focus = Focus::Settings;
    }

    fn handle_terminal_key(
        &mut self,
        _k: KeyEvent,
        _cmd_tx: &mpsc::UnboundedSender<Cmd>,
        _settings: &Arc<Settings>,
    ) {
    }

    fn handle_search_key(
        &mut self,
        k: KeyEvent,
        _cmd_tx: &mpsc::UnboundedSender<Cmd>,
        _settings: &Arc<Settings>,
    ) {
        match k.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.focus = Focus::Hosts;
            }
            KeyCode::Enter => {
                self.focus = Focus::Hosts;
            }
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
            }
            _ => {}
        }
    }

    fn handle_term_search_key(
        &mut self,
        k: KeyEvent,
        _cmd_tx: &mpsc::UnboundedSender<Cmd>,
        _settings: &Arc<Settings>,
    ) {
        match k.code {
            KeyCode::Esc => {
                if let Some(idx) = self.active_session {
                    if let Some(sess) = self.sessions.get_mut(idx) {
                        sess.search.active = false;
                        sess.search.query.clear();
                        sess.search.matches.clear();
                    }
                }
                self.focus = Focus::Terminal;
            }
            KeyCode::Enter | KeyCode::Down => {
                if let Some(idx) = self.active_session {
                    if let Some(sess) = self.sessions.get_mut(idx) {
                        if !sess.search.matches.is_empty() {
                            sess.search.current =
                                (sess.search.current + 1) % sess.search.matches.len();
                        }
                    }
                }
            }
            KeyCode::Up => {
                if let Some(idx) = self.active_session {
                    if let Some(sess) = self.sessions.get_mut(idx) {
                        if !sess.search.matches.is_empty() {
                            sess.search.current = sess.search.current.saturating_sub(1);
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(idx) = self.active_session {
                    if let Some(sess) = self.sessions.get_mut(idx) {
                        sess.search.query.pop();
                        let raw = sess.view.raw_lines();
                        let lines: Vec<Vec<char>> =
                            raw.iter().map(|l| l.chars().collect()).collect();
                        sess.search.update(&lines);
                    }
                }
            }
            KeyCode::Char(c) => {
                if let Some(idx) = self.active_session {
                    if let Some(sess) = self.sessions.get_mut(idx) {
                        sess.search.query.push(c);
                        let raw = sess.view.raw_lines();
                        let lines: Vec<Vec<char>> =
                            raw.iter().map(|l| l.chars().collect()).collect();
                        sess.search.update(&lines);
                    }
                }
            }
            _ => {}
        }
    }

    fn palette_items(&self) -> Vec<(String, String)> {
        let mut items: Vec<(String, String)> = vec![
            ("New host".into(), "n".into()),
            ("Edit host".into(), "e".into()),
            ("Delete host".into(), "d".into()),
            ("Import SSH config".into(), "i".into()),
            ("Save config".into(), "s".into()),
            ("Toggle group view".into(), "g".into()),
            ("Search hosts".into(), "/".into()),
            ("New terminal".into(), "ctrl+n".into()),
            ("Close terminal".into(), "ctrl+w".into()),
            ("Toggle capture".into(), "ctrl+b".into()),
            ("Toggle mouse".into(), "alt+m".into()),
            ("Quit".into(), "q".into()),
        ];

        if matches!(self.update_status, UpdateStatus::Available) && !self.update_dismissed {
            items.push(("Update".into(), "update".into()));
        }

        for m in &self.settings.macros {
            let label = format!("Run: {}", m.name);
            items.push((label, format!("macro:{}", m.name)));
        }
        items
    }

    fn handle_palette_key(
        &mut self,
        k: KeyEvent,
        cmd_tx: &mpsc::UnboundedSender<Cmd>,
        settings: &Arc<Settings>,
    ) {
        let items = self.palette_items();
        let filtered: Vec<(usize, &(String, String))> = items
            .iter()
            .enumerate()
            .filter(|(_, (label, _))| {
                self.palette_filter.is_empty()
                    || label
                        .to_lowercase()
                        .contains(&self.palette_filter.to_lowercase())
            })
            .collect();

        match k.code {
            KeyCode::Esc => {
                self.focus = Focus::Hosts;
            }
            KeyCode::Enter => {
                if filtered.is_empty() {
                    return;
                }
                let (_, (_, key)) = &filtered[self.palette_selected.min(filtered.len() - 1)];
                self.focus = Focus::Hosts;

                if let Some(macro_name) = key.strip_prefix("macro:") {
                    if let Some(m) = self.settings.macros.iter().find(|m| m.name == macro_name) {
                        if let Some(idx) = self.active_session {
                            if let Some(sess) = self.sessions.get_mut(idx) {
                                for cmd in &m.commands {
                                    if let Some(tx) = sess.cmd_tx.as_ref() {
                                        let mut bytes = cmd.as_bytes().to_vec();
                                        bytes.push(b'\n');
                                        let _ = tx.send(SessionCmd::Send(bytes));
                                    }
                                }
                                self.push_toast(
                                    format!("macro: {} ({} cmds)", m.name, m.commands.len()),
                                    MsgLevel::Info,
                                );
                            }
                        }
                    }
                    return;
                }

                let mapped = match key.as_str() {
                    "n" => KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
                    "e" => KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
                    "d" => KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
                    "i" => KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
                    "s" => KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
                    "g" => KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
                    "/" => KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
                    "ctrl+n" => KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
                    "ctrl+w" => KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
                    "ctrl+b" => KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
                    "alt+m" => KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT),
                    "q" => KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                    "update" => KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE),
                    _ => return,
                };
                self.handle_key(mapped, cmd_tx, settings);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if !filtered.is_empty() {
                    self.palette_selected = self.palette_selected.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !filtered.is_empty() {
                    self.palette_selected = (self.palette_selected + 1).min(filtered.len() - 1);
                }
            }
            KeyCode::Backspace => {
                self.palette_filter.pop();
                self.palette_selected = 0;
            }
            KeyCode::Char(c) => {
                self.palette_filter.push(c);
                self.palette_selected = 0;
            }
            _ => {}
        }
    }

    fn handle_prompt_key(&mut self, k: KeyEvent, settings: &Arc<Settings>) {
        let (buf, cur) = if let AppMode::Prompt { buffer, cursor, .. } = &mut self.mode {
            (buffer, cursor)
        } else {
            return;
        };
        match k.code {
            KeyCode::Esc => {
                let is_rename = matches!(
                    &self.mode,
                    AppMode::Prompt {
                        kind: PromptKind::RenameSession { .. },
                        ..
                    }
                );
                let is_settings = matches!(
                    &self.mode,
                    AppMode::Prompt {
                        kind: PromptKind::KeybindingEdit { .. }
                            | PromptKind::KeybindingNew
                            | PromptKind::MacroName { .. }
                            | PromptKind::MacroCmds { .. },
                        ..
                    }
                );
                if matches!(
                    &self.mode,
                    AppMode::Prompt {
                        kind: PromptKind::SftpMkdir { .. }
                            | PromptKind::SftpRename { .. }
                            | PromptKind::SftpFilter { .. },
                        ..
                    }
                ) {
                    self.mode = AppMode::Browsing;
                    return;
                }
                self.mode = AppMode::Browsing;
                self.edit_target = None;
                self.pending_dial = None;
                self.pending_host_opts = None;
                self.pending_macro_name = None;
                self.focus = if is_rename {
                    Focus::Terminal
                } else if is_settings {
                    Focus::Settings
                } else {
                    Focus::Hosts
                };
            }
            KeyCode::Enter => {
                let value = buf.clone();
                let kind = if let AppMode::Prompt { kind, .. } = &self.mode {
                    kind.clone()
                } else {
                    return;
                };
                self.submit_prompt(kind, value, settings);
            }
            KeyCode::Backspace => {
                if *cur > 0 {
                    let prev = buf[..*cur]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    buf.remove(prev);
                    *cur = prev;
                }
            }
            KeyCode::Left => {
                if *cur > 0 {
                    *cur = buf[..*cur]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
            }
            KeyCode::Right => {
                if *cur < buf.len() {
                    *cur = buf[*cur..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| *cur + i)
                        .unwrap_or(buf.len());
                }
            }
            KeyCode::Char(c) => {
                let len = c.len_utf8();
                buf.insert(*cur, c);
                *cur += len;
            }
            _ => {}
        }
    }

    fn submit_prompt(&mut self, kind: PromptKind, value: String, settings: &Arc<Settings>) {
        match kind {
            PromptKind::NewHost => {
                if let Some((user, host, port)) = parse_user_host(&value) {
                    let h = Host {
                        name: host.clone(),
                        host,
                        port,
                        user,
                        identity: Vec::new(),
                        jump: None,
                        tags: Vec::new(),
                        group: None,
                        keepalive: None,
                        on_connect: Vec::new(),
                        forwarding: Vec::new(),
                    };
                    self.hosts.push(h);
                    self.host_state.select(Some(self.hosts.len() - 1));
                    self.save_config(settings);
                    self.status_msg = Some("host added".into());

                    if let Some(h) = self.selected_host().cloned() {
                        self.mode = AppMode::Prompt {
                            kind: PromptKind::EditField {
                                host_name: h.name,
                                field: EditField::KeyPath,
                                original: String::new(),
                            },
                            buffer: String::new(),
                            cursor: 0,
                        };
                        return;
                    }
                }
                self.mode = AppMode::Browsing;
            }
            PromptKind::DeleteConfirm { host } => {
                if value.eq_ignore_ascii_case("y") {
                    self.hosts.retain(|h| h.name != host);
                    self.save_config(settings);
                    self.status_msg = Some(format!("deleted '{}'", host));
                }
                self.mode = AppMode::Browsing;
            }
            PromptKind::MasterPassword => {
                let master_pwd = value;
                if vault_exists() {
                    match vault_load(&master_pwd) {
                        Ok(v) => {
                            self.master_vault = Some(v);
                            self.master_password = Some(master_pwd.clone());
                            self.status_msg = Some("vault loaded".into());
                        }
                        Err(e) => {
                            self.status_msg = Some(format!("vault load error: {}", e));

                            self.mode = AppMode::Prompt {
                                kind: PromptKind::MasterPassword,
                                buffer: String::new(),
                                cursor: 0,
                            };
                            return;
                        }
                    }
                } else {
                    match vault_create(&master_pwd) {
                        Ok(v) => {
                            self.master_vault = Some(v);
                            self.master_password = Some(master_pwd.clone());
                            self.status_msg = Some("vault created".into());
                        }
                        Err(e) => {
                            self.status_msg = Some(format!("vault create error: {}", e));
                            self.mode = AppMode::Prompt {
                                kind: PromptKind::MasterPassword,
                                buffer: String::new(),
                                cursor: 0,
                            };
                            return;
                        }
                    }
                }
                self.mode = AppMode::Browsing;
                self.focus = Focus::Hosts;
            }
            PromptKind::Password { host: _ } => {
                if let Some((host_name, opts)) = self.pending_host_opts.take() {
                    if value.is_empty() {
                        self.status_msg = Some("password empty, cancelled".into());
                        self.mode = AppMode::Browsing;
                        return;
                    }

                    if let Some(vault) = self.master_vault.as_mut() {
                        let id = host_id(&opts.host, &opts.user);
                        let mut secret = vault.get(&id).cloned().unwrap_or_default();
                        secret.password = value.clone();
                        vault.set(&id, secret);
                        if let Some(master_pwd) = &self.master_password {
                            if let Err(e) = betterssh_core::vault_save(vault, master_pwd) {
                                self.status_msg = Some(format!("vault save error: {}", e));
                            }
                        }
                    }
                    self.last_entered_password = Some(value.clone());
                    self.start_connect(host_name, opts);
                    if let Some(d) = self.pending_dial.as_ref() {
                        let _ = d.pw_tx.send(value);
                    }
                    return;
                }
                if let Some(d) = self.pending_dial.take() {
                    if value.is_empty() {
                        self.status_msg = Some("password empty, cancelled".into());
                        return;
                    }
                    self.last_entered_password = Some(value.clone());
                    let _ = d.pw_tx.send(value);
                    self.status_msg = Some("authenticating...".into());
                } else {
                    self.mode = AppMode::Browsing;
                    self.status_msg = Some("no pending password request".into());
                }
            }
            PromptKind::JumpPassword { via, dest: _ } => {
                if let Some((host_name, opts)) = self.pending_host_opts.take() {
                    if !value.is_empty() {
                        self.last_entered_password = Some(value.clone());
                        let mut opts = opts;
                        opts.jump.push(ConnectOpts {
                            host: via,
                            port: 22,
                            user: opts.user.clone(),
                            auth: vec![AuthChoice::Password(value)],
                            term_cols: 0,
                            term_rows: 0,
                            term_type: String::new(),
                            keepalive_secs: None,
                            jump: Vec::new(),
                            use_agent: false,
                        });
                        self.start_connect(host_name, opts);
                        return;
                    }
                }
                self.mode = AppMode::Browsing;
            }
            PromptKind::Passphrase { path } => {
                if let Some((host_name, mut opts)) = self.pending_host_opts.take() {
                    opts.auth = opts
                        .auth
                        .clone()
                        .into_iter()
                        .map(|a| match a {
                            AuthChoice::KeyFile {
                                path: p,
                                passphrase: _,
                            } if p == path => AuthChoice::KeyFile {
                                path: p,
                                passphrase: Some(value.clone()),
                            },
                            other => other,
                        })
                        .collect();
                    if let Some(vault) = self.master_vault.as_mut() {
                        let id = host_id(&opts.host, &opts.user);
                        let mut secret = vault.get(&id).cloned().unwrap_or_default();
                        secret.key_passphrase.insert(path, value);
                        vault.set(&id, secret);
                        if let Some(master_pwd) = &self.master_password {
                            if let Err(e) = betterssh_core::vault_save(vault, master_pwd) {
                                self.status_msg = Some(format!("vault save error: {}", e));
                            }
                        }
                    }
                    self.start_connect(host_name, opts);
                    return;
                }
                self.mode = AppMode::Browsing;
            }
            PromptKind::EditField {
                host_name,
                field,
                original: _,
            } => {
                let val = value;
                if let Some(h) = self.find_host_mut(&host_name) {
                    apply_edit(h, &field, &val);
                }
                let next = next_field(&field);
                match next {
                    Some(f) => {
                        if let Some(h) = self.find_host(&host_name) {
                            let original = current_field_value(h, &f);
                            self.mode = AppMode::Prompt {
                                kind: PromptKind::EditField {
                                    host_name: host_name.clone(),
                                    field: f,
                                    original: original.clone(),
                                },
                                buffer: original.clone(),
                                cursor: original.len(),
                            };
                        } else {
                            self.mode = AppMode::Browsing;
                            self.edit_target = None;
                            self.save_config(settings);
                            self.status_msg = Some(format!("saved '{}'", host_name));
                        }
                    }
                    None => {
                        self.mode = AppMode::Browsing;
                        self.edit_target = None;
                        self.save_config(settings);
                        self.status_msg = Some(format!("saved '{}'", host_name));
                    }
                }
            }
            PromptKind::SftpMkdir { session_id } => {
                if !value.is_empty() {
                    if let Some(sid) = self.sftp_session_id {
                        if sid == session_id {
                            if let Some(idx) = self.session_index(sid) {
                                if let Some(s) = self.sessions[idx].sftp_state.as_ref() {
                                    let path = format!(
                                        "{}/{}",
                                        s.remote_path.trim_end_matches('/'),
                                        value
                                    );
                                    let remote_path = s.remote_path.clone();
                                    let handle = self.sessions[idx].handle.clone().unwrap();
                                    let (tx, rx) = mpsc::unbounded_channel::<Vec<SftpEntry>>();
                                    tokio::spawn(async move {
                                        if let Ok(fs) = open_remote(&handle).await {
                                            let _ = fs.mkdir(&path).await;
                                            if let Ok(list) = fs.list(&remote_path).await {
                                                let _ = tx.send(
                                                    list.into_iter()
                                                        .map(|e| SftpEntry {
                                                            name: e.name,
                                                            is_dir: e.is_dir,
                                                            size: e.size,
                                                        })
                                                        .collect(),
                                                );
                                            }
                                        }
                                    });
                                    self.sessions[idx].sftp_rx = Some(rx);
                                    self.status_msg = Some(format!("mkdir: {}", value));
                                }
                            }
                        }
                    }
                }
                self.restore_sftp_or_browse();
            }
            PromptKind::SftpRename { session_id } => {
                if !value.is_empty() {
                    if let Some(sid) = self.sftp_session_id {
                        if sid == session_id {
                            if let Some(idx) = self.session_index(sid) {
                                let (old, new, remote_path, entry_name) = {
                                    let s = self.sessions[idx].sftp_state.as_ref().unwrap();
                                    let entry_name =
                                        s.current_entries().get(s.sel).map(|e| e.name.clone());
                                    match entry_name {
                                        Some(ref name) => {
                                            let old = format!(
                                                "{}/{}",
                                                s.remote_path.trim_end_matches('/'),
                                                name
                                            );
                                            let new = format!(
                                                "{}/{}",
                                                s.remote_path.trim_end_matches('/'),
                                                value
                                            );
                                            (old, new, s.remote_path.clone(), name.clone())
                                        }
                                        None => return self.restore_sftp_or_browse(),
                                    }
                                };
                                let handle = self.sessions[idx].handle.clone().unwrap();
                                let (tx, rx) = mpsc::unbounded_channel::<Vec<SftpEntry>>();
                                tokio::spawn(async move {
                                    if let Ok(fs) = open_remote(&handle).await {
                                        let _ = fs.rename(&old, &new).await;
                                        if let Ok(list) = fs.list(&remote_path).await {
                                            let _ = tx.send(
                                                list.into_iter()
                                                    .map(|e| SftpEntry {
                                                        name: e.name,
                                                        is_dir: e.is_dir,
                                                        size: e.size,
                                                    })
                                                    .collect(),
                                            );
                                        }
                                    }
                                });
                                self.sessions[idx].sftp_rx = Some(rx);
                                self.status_msg =
                                    Some(format!("rename: {} -> {}", entry_name, value));
                            }
                        }
                    }
                }
                self.restore_sftp_or_browse();
            }
            PromptKind::SftpFilter { session_id } => {
                if let Some(idx) = self.session_index(session_id) {
                    if let Some(s) = self.sessions[idx].sftp_state.as_mut() {
                        s.filter = value;
                        s.sel = 0;
                    }
                }
                self.restore_sftp_or_browse();
            }
            PromptKind::RenameSession { session_idx } => {
                if !value.is_empty() {
                    if let Some(s) = self.sessions.get_mut(session_idx) {
                        s.label = value;
                    }
                }
                self.mode = AppMode::Browsing;
                self.focus = Focus::Terminal;
            }
            PromptKind::KeybindingEdit { action, current: _ } => {
                if value.is_empty() {
                    self.settings.keybindings.remove(&action);
                } else {
                    self.settings.keybindings.insert(action, value);
                }
                if let Some(sf) = &mut self.settings_focus {
                    sf.rebuild_section("Keybindings", &self.settings);
                    sf.modified = true;
                }
                self.mode = AppMode::Browsing;
                self.focus = Focus::Settings;
            }
            PromptKind::KeybindingNew => {
                if let Some((action, key)) = value.split_once('=') {
                    let a = action.trim().to_string();
                    let k = key.trim().to_string();
                    if !a.is_empty() && !k.is_empty() {
                        self.settings.keybindings.insert(a, k);
                    }
                }
                if let Some(sf) = &mut self.settings_focus {
                    sf.rebuild_section("Keybindings", &self.settings);
                    sf.modified = true;
                }
                self.mode = AppMode::Browsing;
                self.focus = Focus::Settings;
            }
            PromptKind::MacroName { current: _ } => {
                if value.is_empty() {
                    if let Some(target) = self.edit_target.take() {
                        if let Some(idx_str) = target.strip_prefix("macro_") {
                            if let Ok(idx) = idx_str.parse::<usize>() {
                                if idx < self.settings.macros.len() {
                                    self.settings.macros.remove(idx);
                                }
                            }
                        }
                    }
                    if let Some(sf) = &mut self.settings_focus {
                        sf.rebuild_section("Macros", &self.settings);
                        sf.modified = true;
                    }
                    self.mode = AppMode::Browsing;
                    self.focus = Focus::Settings;
                } else {
                    let target = self.edit_target.take();
                    let idx_opt = target.and_then(|t| {
                        t.strip_prefix("macro_")
                            .and_then(|s| s.parse::<usize>().ok())
                    });
                    self.pending_macro_name = Some((value.clone(), idx_opt));
                    let cmds = idx_opt
                        .and_then(|i| self.settings.macros.get(i))
                        .map(|m| m.commands.join("; "))
                        .unwrap_or_default();
                    self.mode = AppMode::Prompt {
                        kind: PromptKind::MacroCmds {
                            name: value,
                            current_cmds: cmds.clone(),
                        },
                        buffer: cmds.clone(),
                        cursor: cmds.len(),
                    };
                }
            }
            PromptKind::MacroCmds {
                name,
                current_cmds: _,
            } => {
                if let Some((_name, idx_opt)) = self.pending_macro_name.take() {
                    let cmds: Vec<String> = value
                        .split(';')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    let m = betterssh_core::Macro {
                        name,
                        commands: cmds,
                        key: None,
                    };
                    if let Some(idx) = idx_opt {
                        if idx < self.settings.macros.len() {
                            self.settings.macros[idx] = m;
                        }
                    } else {
                        self.settings.macros.push(m);
                    }
                }
                if let Some(sf) = &mut self.settings_focus {
                    sf.rebuild_section("Macros", &self.settings);
                    sf.modified = true;
                }
                self.mode = AppMode::Browsing;
                self.focus = Focus::Settings;
            }
        }
    }

    fn restore_sftp_or_browse(&mut self) {
        if self.sftp_session_id.is_some() {
            self.mode = AppMode::Sftp;
            self.focus = Focus::Sftp;
        } else {
            self.mode = AppMode::Browsing;
            self.focus = Focus::Terminal;
        }
    }

    fn collect_metrics(&mut self) {
        if let Ok(stat) = std::fs::read_to_string("/proc/stat") {
            if let Some(line) = stat.lines().next() {
                let parts: Vec<u64> = line
                    .split_whitespace()
                    .skip(1)
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if parts.len() >= 4 {
                    let work: u64 = parts[0..3].iter().sum();
                    let idle = parts[3];
                    let total = work + idle;
                    let prev_total = self.prev_cpu_work + self.prev_cpu_idle;
                    if total > prev_total {
                        let delta = total - prev_total;
                        self.metrics.cpu_pct =
                            ((work - self.prev_cpu_work) as f32 / delta as f32) * 100.0;
                        self.prev_cpu_work = work;
                        self.prev_cpu_idle = idle;
                    }
                }
            }
        }

        if let Ok(mem) = std::fs::read_to_string("/proc/meminfo") {
            let mut total_kb: u64 = 0;
            let mut avail_kb: u64 = 0;
            for line in mem.lines() {
                if line.starts_with("MemTotal:") {
                    total_kb = line
                        .split_whitespace()
                        .nth(1)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                } else if line.starts_with("MemAvailable:") {
                    avail_kb = line
                        .split_whitespace()
                        .nth(1)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                }
            }
            self.metrics.ram_total_mb = total_kb / 1024;
            self.metrics.ram_used_mb = (total_kb.saturating_sub(avail_kb)) / 1024;
        }

        if let Ok(la) = std::fs::read_to_string("/proc/loadavg") {
            let parts: Vec<&str> = la.split_whitespace().collect();
            if parts.len() >= 3 {
                self.metrics.load_1 = parts[0].parse().unwrap_or(0.0);
                self.metrics.load_5 = parts[1].parse().unwrap_or(0.0);
                self.metrics.load_15 = parts[2].parse().unwrap_or(0.0);
            }
        }

        if let Ok(ut) = std::fs::read_to_string("/proc/uptime") {
            if let Some(first) = ut.split_whitespace().next() {
                self.metrics.uptime_secs = first.parse::<f64>().unwrap_or(0.0) as u64;
            }
        }

        {
            let stat = std::fs::metadata("/");
            if let Ok(_st) = stat {}
        }

        if let Ok(df) = std::process::Command::new("df").args(["-B1", "/"]).output() {
            if let Some(line) = String::from_utf8_lossy(&df.stdout).lines().nth(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    let total: u64 = parts[1].parse().unwrap_or(0);
                    let used: u64 = parts[2].parse().unwrap_or(0);
                    self.metrics.disk_total_gb = total as f32 / 1073741824.0;
                    self.metrics.disk_used_gb = used as f32 / 1073741824.0;
                }
            }
        }

        if let Ok(nd) = std::fs::read_to_string("/proc/net/dev") {
            let mut rx_total: u64 = 0;
            let mut tx_total: u64 = 0;
            for line in nd.lines().skip(2) {
                if let Some((_iface, rest)) = line.split_once(':') {
                    let nums: Vec<u64> = rest
                        .split_whitespace()
                        .take(10)
                        .filter_map(|s| s.parse().ok())
                        .collect();
                    if nums.len() >= 10 {
                        rx_total += nums[0];
                        tx_total += nums[8];
                    }
                }
            }
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(self.prev_net_time).as_secs_f32();
            if elapsed > 0.5 && self.prev_net_time != std::time::Instant::now() {
                self.metrics.net_down_kbs =
                    (rx_total.saturating_sub(self.prev_net_rx)) as f32 / elapsed / 1024.0;
                self.metrics.net_up_kbs =
                    (tx_total.saturating_sub(self.prev_net_tx)) as f32 / elapsed / 1024.0;
                self.prev_net_rx = rx_total;
                self.prev_net_tx = tx_total;
                self.prev_net_time = now;
            }
            if self.prev_net_rx == 0 && self.prev_net_tx == 0 {
                self.prev_net_rx = rx_total;
                self.prev_net_tx = tx_total;
                self.prev_net_time = now;
            }
        }

        self.metrics.cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        self.check_hosts_alive();

        if let Some(rx) = self.remote_metrics_rx.as_mut() {
            match rx.try_recv() {
                Ok(Some(metrics)) => {
                    self.remote_metrics = Some(metrics);
                    self.remote_metrics_rx = None;
                }
                Ok(None) => {
                    self.remote_metrics_rx = None;
                }
                _ => {}
            }
        }

        if self.remote_metrics_rx.is_none() {
            if let Some(idx) = self.active_session {
                if let Some(s) = self.sessions.get(idx) {
                    if let Some(handle) = s.handle.clone() {
                        if self.last_remote_metrics_collect.elapsed() >= Duration::from_secs(2) {
                            self.last_remote_metrics_collect = Instant::now();
                            let (tx, rx) = tokio::sync::oneshot::channel::<Option<RemoteMetrics>>();
                            self.remote_metrics_rx = Some(rx);
                            tokio::spawn(async move {
                                let metrics = collect_remote_metrics(handle).await;
                                let _ = tx.send(metrics);
                            });
                        }
                    }
                }
            }
        }
    }

    fn check_hosts_alive(&mut self) {
        let mut i = 0;
        while i < self.pending_host_checks.len() {
            let (name, rx) = &self.pending_host_checks[i];
            match rx.try_recv() {
                Ok(Ok(_)) => {
                    self.host_status.insert(name.clone(), HostStatus::Alive);
                    self.pending_host_checks.swap_remove(i);
                }
                Ok(Err(e)) => {
                    self.host_status.insert(name.clone(), HostStatus::Dead(e));
                    self.pending_host_checks.swap_remove(i);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    i += 1;
                }
                Err(_) => {
                    self.pending_host_checks.swap_remove(i);
                }
            }
        }

        if self.last_host_check.elapsed() <= std::time::Duration::from_secs(30) {
            return;
        }
        self.last_host_check = std::time::Instant::now();
        for h in &self.hosts {
            let name = h.name.clone();
            if self.host_status.get(&name) == Some(&HostStatus::Alive) {
                continue;
            }

            if self.pending_host_checks.iter().any(|(n, _)| n == &name) {
                continue;
            }
            let addr_str = format!("{}:{}", h.host, h.port);
            let (tx, rx) = std::sync::mpsc::channel();
            self.pending_host_checks.push((name.clone(), rx));
            std::thread::spawn(
                move || match std::net::ToSocketAddrs::to_socket_addrs(&addr_str) {
                    Ok(addrs) => {
                        for addr in addrs {
                            if std::net::TcpStream::connect_timeout(
                                &addr,
                                std::time::Duration::from_secs(2),
                            )
                            .is_ok()
                            {
                                let _ = tx.send(Ok::<(), String>(()));
                                return;
                            }
                        }
                        let _ = tx.send(Err("unreachable".into()));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("{}", e)));
                    }
                },
            );
        }
    }

    fn on_tick(&mut self) {
        self.update_status = if update::is_downloading() {
            UpdateStatus::Downloading
        } else if update::is_done() {
            UpdateStatus::Done
        } else if update::is_available() && !self.update_dismissed {
            UpdateStatus::Available
        } else if let Some(e) = update::error() {
            UpdateStatus::Failed(e)
        } else if update::is_checking() {
            UpdateStatus::Checking
        } else {
            UpdateStatus::Idle
        };
        if let Some(v) = update::latest_version() {
            self.update_latest_version = v.to_string();
        }

        if let Some((cols, rows)) = self.pending_resize.take() {
            for s in &mut self.sessions {
                s.view.resize(cols.max(20), rows.max(5));
                s.tx_cols = cols.max(20);
                s.tx_rows = rows.max(5);
                if let Some(tx) = s.cmd_tx.as_ref() {
                    let _ = tx.send(SessionCmd::Resize(cols, rows));
                }
            }
            self.term_cols = cols.max(20);
            self.term_rows = rows.max(5);
        }

        self.collect_metrics();

        let mut new_toasts: Vec<(String, MsgLevel)> = Vec::new();

        for s in self.sessions.iter_mut() {
            while let Ok(evt) = s.events.try_recv() {
                match evt {
                    SshEvent::Data(b) => s.view.feed(&b),
                    SshEvent::Disconnected(reason) => {
                        let already = matches!(s.status, SessionStatus::Disconnected(_));
                        if !already {
                            s.status = SessionStatus::Disconnected(reason.clone());
                            new_toasts.push((
                                format!("{} disconnected: {}", s.host_name, reason),
                                MsgLevel::Warn,
                            ));
                            cleanup_forwards(s);
                        }
                    }
                    SshEvent::Exit(code) => {
                        s.status = SessionStatus::Disconnected(format!("exit {}", code));
                        new_toasts.push((format!("{} exit {}", s.host_name, code), MsgLevel::Info));
                        cleanup_forwards(s);
                    }
                    SshEvent::Error(e) => {
                        new_toasts.push((format!("{} err: {}", s.host_name, e), MsgLevel::Bad));
                    }
                    SshEvent::Log(line) => {
                        new_toasts.push((line, MsgLevel::Info));
                    }
                    SshEvent::Connected => {}
                }
            }

            if let Some(rx) = s.sftp_rx.as_mut() {
                if let Ok(entries) = rx.try_recv() {
                    if let Some(sftp) = s.sftp_state.as_mut() {
                        sftp.remote_entries = entries;
                    }
                    s.sftp_rx = None;
                }
            }
            if let Some(rx) = s.sftp_result_rx.as_mut() {
                if let Ok(result) = rx.try_recv() {
                    match result {
                        Ok(()) => new_toasts.push(("ok".into(), MsgLevel::Info)),
                        Err(e) => new_toasts.push((e, MsgLevel::Bad)),
                    }
                    s.sftp_result_rx = None;
                }
            }

            if let Some(sid) = self.sftp_session_id {
                if s.id == sid {
                    if let Some(sftp) = s.sftp_state.as_mut() {
                        sftp.refresh_local();
                    }
                }
            }
        }

        if let AppMode::Message { until, .. } = &self.mode {
            if Instant::now() >= *until {
                self.mode = AppMode::Browsing;
            }
        }

        for (text, level) in new_toasts {
            self.push_toast(text, level);
        }
    }

    async fn handle_cmd(&mut self, cmd: Cmd, _settings: &Arc<Settings>) {
        match cmd {
            Cmd::Connect { host } => {
                let mut opts = build_opts(&host);
                let (real_cols, real_rows) = crossterm::terminal::size().unwrap_or((120, 32));
                opts.term_cols = real_cols.saturating_sub(2).max(20);
                opts.term_rows = real_rows.saturating_sub(3).max(5);

                if host.identity.iter().any(|i| matches!(i, Identity::Agent)) {
                    self.push_toast("agent: loading keys from SSH_AUTH_SOCK", MsgLevel::Info);
                }

                let needs_pp = opts.auth.iter().any(|a| {
                    matches!(
                        a,
                        AuthChoice::KeyFile {
                            passphrase: None,
                            ..
                        }
                    )
                });
                let stored_pp = if needs_pp {
                    self.master_vault.as_ref().and_then(|v| {
                        let id = host_id(&host.host, &host.user);
                        v.get(&id).and_then(|s| {
                            opts.auth.iter().find_map(|a| {
                                if let AuthChoice::KeyFile {
                                    path,
                                    passphrase: None,
                                } = a
                                {
                                    s.key_passphrase
                                        .get(path)
                                        .filter(|p| !p.is_empty())
                                        .cloned()
                                } else {
                                    None
                                }
                            })
                        })
                    })
                } else {
                    None
                };
                if let Some(ref pp) = stored_pp {
                    opts.auth.iter_mut().for_each(|a| {
                        if let AuthChoice::KeyFile { passphrase, .. } = a {
                            if passphrase.is_none() {
                                *passphrase = Some(pp.clone());
                            }
                        }
                    });
                }

                if let Some(jump_name) = &host.jump {
                    if let Some(jump_host) = self.hosts.iter().find(|h| h.name == *jump_name) {
                        let jump_opts = build_opts(jump_host);
                        opts.jump.push(ConnectOpts {
                            host: jump_host.host.clone(),
                            port: jump_host.port,
                            user: jump_host.user.clone(),
                            auth: jump_opts.auth,
                            term_cols: 0,
                            term_rows: 0,
                            term_type: String::new(),
                            keepalive_secs: jump_host.keepalive,
                            jump: Vec::new(),
                            use_agent: jump_opts.use_agent,
                        });
                    } else {
                        self.push_toast(
                            format!("jump host '{}' not found", jump_name),
                            MsgLevel::Bad,
                        );
                        return;
                    }
                }

                let needs_password = host
                    .identity
                    .iter()
                    .any(|i| matches!(i, Identity::Password { .. }));
                let stored_password = if needs_password {
                    self.master_vault.as_ref().and_then(|v| {
                        let id = host_id(&host.host, &host.user);
                        v.get(&id).and_then(|s| {
                            if s.password.is_empty() {
                                None
                            } else {
                                Some(s.password.clone())
                            }
                        })
                    })
                } else {
                    None
                };

                if needs_pp && stored_pp.is_none() {
                    if let Some(path) = opts.auth.iter().find_map(|a| {
                        if let AuthChoice::KeyFile {
                            path,
                            passphrase: None,
                        } = a
                        {
                            Some(path.clone())
                        } else {
                            None
                        }
                    }) {
                        self.mode = AppMode::Prompt {
                            kind: PromptKind::Passphrase { path },
                            buffer: String::new(),
                            cursor: 0,
                        };
                        self.pending_host_opts = Some((host.name.clone(), opts));
                        return;
                    }
                }

                if needs_password && stored_password.is_none() {
                    self.mode = AppMode::Prompt {
                        kind: PromptKind::Password {
                            host: format!("{}@{}:{}", host.user, host.host, host.port),
                        },
                        buffer: String::new(),
                        cursor: 0,
                    };
                    self.pending_host_opts = Some((host.name.clone(), opts));
                    return;
                }

                self.start_connect(host.name.clone(), opts);
            }
            Cmd::CancelConnect => {
                self.mode = AppMode::Browsing;
                self.status_msg = Some("connect cancelled".into());
            }
            Cmd::OpenSftp { session_id } => {
                if let Some(idx) = self.session_index(session_id) {
                    self.open_sftp_for_session(idx);
                }
            }
            Cmd::SftpEnter => {
                self.sftp_enter();
            }
            Cmd::SftpUpDir => {
                self.sftp_up_dir();
            }
            Cmd::SftpUpload => {
                self.sftp_upload();
            }
            Cmd::SftpDownload => {
                self.sftp_download();
            }
            Cmd::SftpDelete => {
                self.sftp_delete();
            }
            Cmd::PortForwardStart { session_id, fw_id } => {
                self.start_forward(session_id, fw_id).await;
            }
            Cmd::PortForwardStop { session_id, fw_id } => {
                self.stop_forward(session_id, fw_id).await;
            }
        }
    }

    async fn start_forward(&mut self, session_id: SessionId, fw_id: u64) {
        let idx = match self.session_index(session_id) {
            Some(i) => i,
            None => return,
        };
        let handle = match &self.sessions[idx].handle {
            Some(h) => h.clone(),
            None => {
                self.push_toast("not connected", MsgLevel::Bad);
                return;
            }
        };

        let host_name = self.sessions[idx].host_name.clone();
        let fw = match self
            .hosts
            .iter()
            .find(|h| h.name == host_name)
            .and_then(|h| h.forwarding.iter().find(|f| f.id == fw_id))
        {
            Some(f) => f.clone(),
            None => {
                self.push_toast("forward config not found", MsgLevel::Bad);
                return;
            }
        };
        let rf = self.sessions[idx]
            .remote_forwards
            .clone()
            .unwrap_or_else(|| Arc::new(AsyncMutex::new(HashMap::new())));
        match betterssh_ssh::port_forward::start_forward(
            &handle,
            &rf,
            &fw,
            mpsc::unbounded_channel().0,
        )
        .await
        {
            Ok(()) => {
                if let Some(s) = self.sessions.get_mut(idx) {
                    s.forwards.push(ActiveForward {
                        id: fw.id,
                        direction: format!("{}", fw.direction),
                        listen: format!("{}:{}", fw.listen_addr, fw.listen_port),
                        target: if fw.direction == ForwardDirection::Dynamic {
                            "SOCKS".into()
                        } else {
                            format!("{}:{}", fw.target_host, fw.target_port)
                        },
                        active: true,
                        status: "active".into(),
                    });
                }
                if let Some(h) = self.hosts.iter_mut().find(|h| h.name == host_name) {
                    if let Some(f) = h.forwarding.iter_mut().find(|f| f.id == fw_id) {
                        f.active = true;
                    }
                }
                self.push_toast(format!("forward {} started", fw_id), MsgLevel::Info);
            }
            Err(e) => {
                self.push_toast(format!("forward {} error: {}", fw_id, e), MsgLevel::Bad);
            }
        }
    }

    async fn stop_forward(&mut self, session_id: SessionId, fw_id: u64) {
        let idx = match self.session_index(session_id) {
            Some(i) => i,
            None => return,
        };

        if let Some(s) = self.sessions.get_mut(idx) {
            s.forwards.retain(|f| f.id != fw_id);
        }
        let host_name = self
            .sessions
            .get(idx)
            .map(|s| s.host_name.clone())
            .unwrap_or_default();
        if let Some(h) = self.hosts.iter_mut().find(|h| h.name == host_name) {
            if let Some(f) = h.forwarding.iter_mut().find(|f| f.id == fw_id) {
                f.active = false;
            }
        }
        self.push_toast(format!("forward {} stopped", fw_id), MsgLevel::Info);
    }

    fn start_connect(&mut self, host_name: String, mut opts: ConnectOpts) {
        let (real_cols, real_rows) = crossterm::terminal::size().unwrap_or((120, 32));
        opts.term_cols = real_cols.saturating_sub(2).max(20);
        opts.term_rows = real_rows.saturating_sub(4).max(5);

        let session_id = self.alloc_session_id();
        let label = format!("{}@{}:{}", opts.user, opts.host, opts.port);
        let mut sess = Session::new(
            session_id,
            host_name.clone(),
            label,
            opts.term_cols,
            opts.term_rows,
        );
        sess.status = SessionStatus::Connecting;
        let idx = self.sessions.len();
        self.sessions.push(sess);
        self.active_session = Some(idx);
        self.dial_session_id = Some(session_id);
        self.focus = Focus::Terminal;

        let dial_tx = self.dial_tx.clone().unwrap();
        let opts_clone = opts.clone();
        let host_name_clone = host_name.clone();

        let has_vault_pw = self.master_vault.as_ref().is_some_and(|v| {
            let id = host_id(&opts.host, &opts.user);
            v.get(&id).is_some_and(|s| !s.password.is_empty())
        });
        let needs_password = has_vault_pw
            || self.last_entered_password.is_some()
            || opts
                .auth
                .iter()
                .any(|a| matches!(a, AuthChoice::Password(_)));

        if needs_password {
            let (pw_tx, mut pw_rx) = mpsc::unbounded_channel::<String>();
            self.pending_dial = Some(PendingDial {
                host_name: host_name.clone(),
                pw_tx,
                session_id,
            });

            if let Some(vault) = self.master_vault.as_ref() {
                let host_id_str = host_id(&opts.host, &opts.user);
                if let Some(secret) = vault.get(&host_id_str) {
                    if !secret.password.is_empty() {
                        if let Some(tx) = self.pending_dial.as_ref().map(|d| d.pw_tx.clone()) {
                            let _ = tx.send(secret.password.clone());
                        }
                    }
                }
            }

            tokio::spawn(async move {
                tracing::debug!("spawn: waiting for password");
                if let Some(pw) = pw_rx.recv().await {
                    tracing::debug!("spawn: got password, connecting");
                    let ask_password = move || Some(pw.clone());
                    match betterssh_ssh::client::connect_with_password(&opts_clone, ask_password)
                        .await
                    {
                        Ok((handle, _events, rf)) => {
                            tracing::debug!("connect OK");
                            let shared = Arc::new(AsyncMutex::new(handle));
                            let _ = dial_tx.send(DialResult::Done(
                                shared,
                                rf,
                                host_name_clone,
                                opts_clone,
                            ));
                        }
                        Err(e) => {
                            tracing::debug!(%e, "connect FAILED");
                            let _ = dial_tx.send(DialResult::Failed(format!("{}", e)));
                        }
                    }
                } else {
                    tracing::debug!("pw_rx closed");
                    let _ = dial_tx.send(DialResult::Failed("cancelled".into()));
                }
            });
        } else {
            tokio::spawn(async move {
                match betterssh_ssh::client::connect(&opts_clone).await {
                    Ok((handle, _events, rf)) => {
                        tracing::debug!("connect OK (key auth)");
                        let shared = Arc::new(AsyncMutex::new(handle));
                        let _ =
                            dial_tx.send(DialResult::Done(shared, rf, host_name_clone, opts_clone));
                    }
                    Err(e) => {
                        tracing::debug!(%e, "connect FAILED");
                        let _ = dial_tx.send(DialResult::Failed(format!("{}", e)));
                    }
                }
            });
        }
    }

    fn open_sftp_for_session(&mut self, idx: usize) {
        if idx >= self.sessions.len() {
            return;
        }
        if self.sessions[idx].handle.is_none() {
            self.push_toast("not connected", MsgLevel::Bad);
            return;
        }
        if self.sessions[idx].sftp_state.is_some() {
            self.sftp_session_id = Some(self.sessions[idx].id);
            self.mode = AppMode::Sftp;
            self.focus = Focus::Sftp;
            return;
        }

        let handle = self.sessions[idx].handle.clone().unwrap();
        let local_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let mut s = SftpState::new(local_path);
        s.refresh_local();

        let sid = self.sessions[idx].id;
        let (sftp_tx, sftp_rx) = mpsc::unbounded_channel::<Vec<SftpEntry>>();
        tokio::spawn(async move {
            let entries = match open_remote(&handle).await {
                Ok(fs) => match fs.list("/").await {
                    Ok(list) => list
                        .into_iter()
                        .map(|e| SftpEntry {
                            name: e.name,
                            is_dir: e.is_dir,
                            size: e.size,
                        })
                        .collect(),
                    Err(_) => vec![],
                },
                Err(_) => vec![],
            };
            let _ = sftp_tx.send(entries);
        });
        self.sessions[idx].sftp_state = Some(s);
        self.sessions[idx].sftp_rx = Some(sftp_rx);
        self.sftp_session_id = Some(sid);
        self.mode = AppMode::Sftp;
        self.focus = Focus::Sftp;
        self.push_toast("sftp open", MsgLevel::Info);
    }

    fn sftp_session_idx(&self) -> Option<usize> {
        let sid = self.sftp_session_id?;
        self.session_index(sid)
    }

    fn sftp_transfer(&mut self, label: &str) {
        let idx = match self.sftp_session_idx() {
            Some(i) => i,
            None => return,
        };
        let entries = self.sessions[idx]
            .sftp_state
            .as_ref()
            .map(|s| s.current_entries().to_vec())
            .unwrap_or_default();
        let (sel, focus, local_path, remote_path) = {
            let s = self.sessions[idx].sftp_state.as_ref().unwrap();
            (s.sel, s.focus, s.local_path.clone(), s.remote_path.clone())
        };
        if sel >= entries.len() || entries[sel].is_dir {
            self.push_toast("select a file".to_string(), MsgLevel::Warn);
            return;
        }
        let entry = &entries[sel];
        let h = self.sessions[idx].handle.clone();
        match focus {
            SftpPane::Local => {
                let local = local_path.join(&entry.name);
                let remote = format!("{}/{}", remote_path.trim_end_matches('/'), entry.name);
                let local_display = local.clone();
                let (tx, rx) = mpsc::unbounded_channel::<Result<(), String>>();
                tokio::spawn(async move {
                    let result = async {
                        let data = tokio::fs::read(&local)
                            .await
                            .map_err(|e| format!("read local: {}", e))?;
                        let h = h.ok_or("not connected")?;
                        let handle = h.lock().await;
                        let fs = RemoteFs::open(&handle)
                            .await
                            .map_err(|e| format!("sftp open: {}", e))?;
                        fs.write_file(&remote, &data)
                            .await
                            .map_err(|e| format!("write remote: {}", e))?;
                        Ok(())
                    }
                    .await;
                    let _ = tx.send(result);
                });
                self.sessions[idx].sftp_result_rx = Some(rx);
                self.push_toast(
                    format!("{}: {} -> remote", label, local_display.display()),
                    MsgLevel::Info,
                );
            }
            SftpPane::Remote => {
                let remote = format!("{}/{}", remote_path.trim_end_matches('/'), entry.name);
                let local = local_path.join(&entry.name);
                let remote_display = remote.clone();
                let (tx, rx) = mpsc::unbounded_channel::<Result<(), String>>();
                tokio::spawn(async move {
                    let result = async {
                        let h = h.ok_or("not connected")?;
                        let handle = h.lock().await;
                        let fs = RemoteFs::open(&handle)
                            .await
                            .map_err(|e| format!("sftp open: {}", e))?;
                        let data = fs
                            .read_file(&remote)
                            .await
                            .map_err(|e| format!("read remote: {}", e))?;
                        drop(handle);
                        if let Some(parent) = local.parent() {
                            tokio::fs::create_dir_all(parent)
                                .await
                                .map_err(|e| format!("mkdir: {}", e))?;
                        }
                        tokio::fs::write(&local, &data)
                            .await
                            .map_err(|e| format!("write local: {}", e))?;
                        Ok(())
                    }
                    .await;
                    let _ = tx.send(result);
                });
                self.sessions[idx].sftp_result_rx = Some(rx);
                self.push_toast(
                    format!("{}: {} -> local", label, remote_display),
                    MsgLevel::Info,
                );
            }
        }
    }

    fn sftp_upload(&mut self) {
        self.sftp_transfer("upload");
    }

    fn sftp_download(&mut self) {
        self.sftp_transfer("download");
    }

    fn sftp_enter(&mut self) {
        let idx = match self.sftp_session_idx() {
            Some(i) => i,
            None => return,
        };
        let entries = self.sessions[idx]
            .sftp_state
            .as_ref()
            .map(|s| s.current_entries().to_vec())
            .unwrap_or_default();
        let (sel, focus, local_path, remote_path) = {
            let s = self.sessions[idx].sftp_state.as_ref().unwrap();
            (s.sel, s.focus, s.local_path.clone(), s.remote_path.clone())
        };
        if sel < entries.len() {
            let entry = &entries[sel];
            if entry.is_dir {
                let p = match focus {
                    SftpPane::Local => local_path.display().to_string(),
                    SftpPane::Remote => remote_path.clone(),
                };
                let new_path = if p == "/" {
                    format!("/{}", entry.name)
                } else {
                    format!("{}/{}", p.trim_end_matches('/'), entry.name)
                };
                let sftp = self.sessions[idx].sftp_state.as_mut().unwrap();
                sftp.set_path(focus, new_path.clone());
                sftp.sel = 0;
                if focus == SftpPane::Local {
                    sftp.refresh_local();
                } else {
                    let handle = self.sessions[idx].handle.clone().unwrap();
                    let (tx, rx) = mpsc::unbounded_channel::<Vec<SftpEntry>>();
                    tokio::spawn(async move {
                        let entries = match open_remote(&handle).await {
                            Ok(fs) => match fs.list(&new_path).await {
                                Ok(list) => list
                                    .into_iter()
                                    .map(|e| SftpEntry {
                                        name: e.name,
                                        is_dir: e.is_dir,
                                        size: e.size,
                                    })
                                    .collect(),
                                Err(_) => vec![],
                            },
                            Err(_) => vec![],
                        };
                        let _ = tx.send(entries);
                    });
                    self.sessions[idx].sftp_rx = Some(rx);
                }
            }
        }
    }

    fn sftp_up_dir(&mut self) {
        let idx = match self.sftp_session_idx() {
            Some(i) => i,
            None => return,
        };
        let (p, focus) = {
            let s = self.sessions[idx].sftp_state.as_ref().unwrap();
            (s.pane_path(s.focus).display(), s.focus)
        };
        if let Some(parent) = parent_path_str(&p) {
            let sftp = self.sessions[idx].sftp_state.as_mut().unwrap();
            sftp.set_path(focus, parent.clone());
            sftp.sel = 0;
            if focus == SftpPane::Local {
                sftp.refresh_local();
            } else {
                let handle = self.sessions[idx].handle.clone().unwrap();
                let (tx, rx) = mpsc::unbounded_channel::<Vec<SftpEntry>>();
                tokio::spawn(async move {
                    let entries = match open_remote(&handle).await {
                        Ok(fs) => match fs.list(&parent).await {
                            Ok(list) => list
                                .into_iter()
                                .map(|e| SftpEntry {
                                    name: e.name,
                                    is_dir: e.is_dir,
                                    size: e.size,
                                })
                                .collect(),
                            Err(_) => vec![],
                        },
                        Err(_) => vec![],
                    };
                    let _ = tx.send(entries);
                });
                self.sessions[idx].sftp_rx = Some(rx);
            }
        }
    }

    fn sftp_delete(&mut self) {
        let idx = match self.sftp_session_idx() {
            Some(i) => i,
            None => return,
        };
        let entries = self.sessions[idx]
            .sftp_state
            .as_ref()
            .map(|s| s.current_entries().to_vec())
            .unwrap_or_default();
        let (sel, focus, local_path, remote_path) = {
            let s = self.sessions[idx].sftp_state.as_ref().unwrap();
            (s.sel, s.focus, s.local_path.clone(), s.remote_path.clone())
        };
        if sel < entries.len() {
            let entry = &entries[sel];
            let p = match focus {
                SftpPane::Local => local_path.display().to_string(),
                SftpPane::Remote => remote_path,
            };
            let path = format!("{}/{}", p.trim_end_matches('/'), entry.name);
            let handle = self.sessions[idx].handle.clone().unwrap();
            let path_clone = path.clone();
            let remote_path = match focus {
                SftpPane::Remote => path.clone(),
                _ => String::new(),
            };
            let (tx, rx) = mpsc::unbounded_channel::<Result<(), String>>();
            let (list_tx, list_rx) = mpsc::unbounded_channel::<Vec<SftpEntry>>();
            tokio::spawn(async move {
                let result = match open_remote(&handle).await {
                    Ok(fs) => match fs.remove(&path_clone).await {
                        Ok(()) => Ok(()),
                        Err(e) => Err(format!("remove: {}", e)),
                    },
                    Err(e) => Err(format!("sftp open: {}", e)),
                };
                let _ = tx.send(result);

                if !remote_path.is_empty() {
                    if let Ok(fs) = open_remote(&handle).await {
                        let parent = parent_path_str(&remote_path).unwrap_or_else(|| "/".into());
                        if let Ok(list) = fs.list(&parent).await {
                            let _ = list_tx.send(
                                list.into_iter()
                                    .map(|e| SftpEntry {
                                        name: e.name,
                                        is_dir: e.is_dir,
                                        size: e.size,
                                    })
                                    .collect(),
                            );
                        }
                    }
                }
            });
            self.sessions[idx].sftp_result_rx = Some(rx);
            self.sessions[idx].sftp_rx = Some(list_rx);
            self.push_toast(format!("delete: {}", path), MsgLevel::Info);
        }
    }
}

async fn open_remote(handle: &SharedHandle) -> Result<RemoteFs, betterssh_ssh::SshError> {
    let h = handle.lock().await;
    RemoteFs::open(&h).await
}

use crate::state::SessionId;

async fn run_shell_loop(
    mut shell_ch: russh::Channel<russh::client::Msg>,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCmd>,
    event_tx: mpsc::UnboundedSender<SshEvent>,
) {
    loop {
        tokio::select! {
            maybe_msg = shell_ch.wait() => {
                match maybe_msg {
                    Some(msg) => match msg {
                        ChannelMsg::Data { data } => {
                            if event_tx.send(SshEvent::Data(data.to_vec())).is_err() {
                                break;
                            }
                        }
                        ChannelMsg::ExtendedData { data, .. } => {
                            if event_tx.send(SshEvent::Data(data.to_vec())).is_err() {
                                break;
                            }
                        }
                        ChannelMsg::ExitStatus { exit_status } => {
                            let _ = event_tx.send(SshEvent::Exit(exit_status as i32));
                            break;
                        }
                        ChannelMsg::Eof => {
                            let _ = event_tx.send(SshEvent::Exit(0));
                            break;
                        }
                        ChannelMsg::Close => {
                            let _ = event_tx.send(SshEvent::Disconnected("closed".into()));
                            break;
                        }
                        _ => {}
                    },
                    None => {
                        let _ = event_tx.send(SshEvent::Disconnected("eof".into()));
                        break;
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SessionCmd::Send(bytes)) => {
                        if let Err(e) = shell_ch.data(bytes.as_slice()).await {
                            tracing::error!("shell_ch.data error: {:?}, bytes={}", e, bytes.iter().map(|b| format!("\\x{:02x}", b)).collect::<String>());
                        }
                    }
                    Some(SessionCmd::Resize(cols, rows)) => {
                        let _ = shell_ch.window_change(cols as u32, rows as u32, 0, 0).await;
                    }
                    Some(SessionCmd::Stop) => {
                        let _ = shell_ch.eof().await;
                        let _ = shell_ch.close().await;
                        break;
                    }
                    None => break,
                }
            }
        }
    }
}

async fn collect_remote_metrics(
    handle: std::sync::Arc<AsyncMutex<SshHandle<ClientHandler>>>,
) -> Option<RemoteMetrics> {
    let command = "echo '---MEM---'; free -b 2>/dev/null | grep 'Mem:'; echo '---DISK---'; df -B1 / 2>/dev/null | tail -1; echo '---LOAD---'; cat /proc/loadavg 2>/dev/null; echo '---UPTIME---'; cat /proc/uptime 2>/dev/null; echo '---CPU---'; nproc 2>/dev/null; echo '---NET---'; cat /proc/net/dev 2>/dev/null";

    let h = handle.lock().await;
    let output = match exec(&h, command).await {
        Ok(o) => o,
        Err(_) => return None,
    };
    drop(h);

    let mut m = RemoteMetrics::default();

    let mut in_mem = false;
    let mut in_disk = false;
    let mut in_load = false;
    let mut in_uptime = false;
    let mut in_cpu = false;
    let mut in_net = false;

    for line in output.lines() {
        if line == "---MEM---" {
            in_mem = true;
            in_disk = false;
            in_load = false;
            in_uptime = false;
            in_cpu = false;
            in_net = false;
            continue;
        }
        if line == "---DISK---" {
            in_mem = false;
            in_disk = true;
            in_load = false;
            in_uptime = false;
            in_cpu = false;
            in_net = false;
            continue;
        }
        if line == "---LOAD---" {
            in_mem = false;
            in_disk = false;
            in_load = true;
            in_uptime = false;
            in_cpu = false;
            in_net = false;
            continue;
        }
        if line == "---UPTIME---" {
            in_mem = false;
            in_disk = false;
            in_load = false;
            in_uptime = true;
            in_cpu = false;
            in_net = false;
            continue;
        }
        if line == "---CPU---" {
            in_mem = false;
            in_disk = false;
            in_load = false;
            in_uptime = false;
            in_cpu = true;
            in_net = false;
            continue;
        }
        if line == "---NET---" {
            in_mem = false;
            in_disk = false;
            in_load = false;
            in_uptime = false;
            in_cpu = false;
            in_net = true;
            continue;
        }

        if in_mem {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 && parts[0] == "Mem:" {
                if let Ok(total) = parts[1].parse::<u64>() {
                    m.ram_total_mb = total / 1048576;
                    if parts.len() >= 7 {
                        if let Ok(avail) = parts[6].parse::<u64>() {
                            m.ram_used_mb = total.saturating_sub(avail) / 1048576;
                        }
                    } else if let Ok(used) = parts[2].parse::<u64>() {
                        m.ram_used_mb = used / 1048576;
                    }
                }
            }
        }
        if in_disk {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                if let Ok(total) = parts[1].parse::<u64>() {
                    m.disk_total_gb = total as f32 / 1073741824.0;
                }
                if let Ok(used) = parts[2].parse::<u64>() {
                    m.disk_used_gb = used as f32 / 1073741824.0;
                }
            }
        }
        if in_load {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                m.load_1 = parts[0].parse().unwrap_or(0.0);
                m.load_5 = parts[1].parse().unwrap_or(0.0);
                m.load_15 = parts[2].parse().unwrap_or(0.0);
            }
        }
        if in_uptime {
            if let Some(first) = line.split_whitespace().next() {
                m.uptime_secs = first.parse::<f64>().unwrap_or(0.0) as u64;
            }
        }
        if in_cpu {
            m.cpu_cores = line.trim().parse().unwrap_or(1);
        }
        if in_net {
            if let Some((_iface, rest)) = line.split_once(':') {
                let nums: Vec<u64> = rest
                    .split_whitespace()
                    .take(10)
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if nums.len() >= 10 {
                    m.net_down_kbs += nums[0] as f32;
                    m.net_up_kbs += nums[8] as f32;
                }
            }
        }
    }

    m.cpu_pct = 0.0;

    Some(m)
}

fn parent_path_str(p: &str) -> Option<String> {
    if p == "/" {
        return None;
    }
    if let Some(idx) = p.rfind('/') {
        if idx == 0 {
            return Some("/".into());
        }
        return Some(p[..idx].to_string());
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn format_duration(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        format!("{}d{}h", days, hours)
    } else if hours > 0 {
        format!("{}h{}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}

fn format_network_speed(kbs: f32) -> String {
    if kbs >= 1048576.0 {
        format!("{:.1}GB/s", kbs / 1048576.0)
    } else if kbs >= 1024.0 {
        format!("{:.1}MB/s", kbs / 1024.0)
    } else if kbs >= 1.0 {
        format!("{:.0}KB/s", kbs)
    } else {
        "0B/s".to_string()
    }
}

fn format_disk(total_gb: f32, used_gb: f32) -> String {
    if total_gb >= 1024.0 {
        format!("{:.1}/{:.1}tb", used_gb / 1024.0, total_gb / 1024.0)
    } else {
        format!("{:.1}/{:.1}gb", used_gb, total_gb)
    }
}

fn prompt_label(kind: &PromptKind, _buf: &str) -> String {
    match kind {
        PromptKind::Password { host } => format!("Password for {}", host),
        PromptKind::MasterPassword => "Master password".into(),
        PromptKind::Passphrase { path } => format!("Passphrase for {}", path),
        PromptKind::NewHost => "New host (user@addr[:port])".into(),
        PromptKind::DeleteConfirm { host } => format!("Delete '{}'? (y/N)", host),
        PromptKind::JumpPassword { via, dest } => format!("Jump password ({} -> {})", via, dest),
        PromptKind::EditField {
            field, host_name, ..
        } => {
            format!(
                "{} of {} (Enter next, Esc cancel)",
                field_label(field),
                host_name
            )
        }
        PromptKind::SftpMkdir { session_id: _ } => "mkdir name".into(),
        PromptKind::SftpRename { session_id: _ } => "rename to".into(),
        PromptKind::SftpFilter { session_id: _ } => "filter".into(),
        PromptKind::RenameSession { .. } => "Rename session".into(),
        PromptKind::KeybindingEdit { action, .. } => {
            format!("Key combo for '{}' (empty=del)", action)
        }
        PromptKind::KeybindingNew => "New binding (action = key)".into(),
        PromptKind::MacroName { .. } => "Macro name".into(),
        PromptKind::MacroCmds { name, .. } => format!("Commands for '{}' (; sep)", name),
    }
}

fn field_label(f: &EditField) -> &'static str {
    match f {
        EditField::Name => "Name",
        EditField::Host => "Host (addr or ip)",
        EditField::Port => "Port",
        EditField::User => "User",
        EditField::Group => "Group",
        EditField::Tags => "Tags (comma sep)",
        EditField::KeyPath => "Key path (empty = no key)",
        EditField::JumpHost => "Jump host name (empty = none)",
        EditField::Password => "Password auth? (y/n)",
        EditField::Keepalive => "Keepalive seconds (0 = off)",
        EditField::OnConnect => "On-connect cmds (; sep)",
        EditField::Forwards => "Port forwards (L:ip:port:target:port|R:...|D:ip:port)",
    }
}

fn current_field_value(h: &Host, f: &EditField) -> String {
    match f {
        EditField::Name => h.name.clone(),
        EditField::Host => h.host.clone(),
        EditField::Port => h.port.to_string(),
        EditField::User => h.user.clone(),
        EditField::Group => h.group.clone().unwrap_or_default(),
        EditField::Tags => h.tags.join(","),
        EditField::KeyPath => h
            .identity
            .iter()
            .find_map(|i| match i {
                Identity::Key { path, .. } => Some(path.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        EditField::JumpHost => h.jump.clone().unwrap_or_default(),
        EditField::Password => {
            let has = h
                .identity
                .iter()
                .any(|i| matches!(i, Identity::Password { .. }));
            if has {
                "y".into()
            } else {
                "n".into()
            }
        }
        EditField::Keepalive => h.keepalive.map(|k| k.to_string()).unwrap_or_default(),
        EditField::OnConnect => h.on_connect.join("; "),
        EditField::Forwards => format_forwards(&h.forwarding),
    }
}

fn format_forwards(fws: &[PortForward]) -> String {
    fws.iter()
        .map(|f| {
            let dir = match f.direction {
                ForwardDirection::Local => "L",
                ForwardDirection::Remote => "R",
                ForwardDirection::Dynamic => "D",
            };
            if f.direction == ForwardDirection::Dynamic {
                format!("{}:{}:{}", dir, f.listen_addr, f.listen_port)
            } else {
                format!(
                    "{}:{}:{}:{}:{}",
                    dir, f.listen_addr, f.listen_port, f.target_host, f.target_port
                )
            }
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn apply_edit(h: &mut Host, field: &EditField, val: &str) {
    match field {
        EditField::Name => h.name = val.to_string(),
        EditField::Host => h.host = val.to_string(),
        EditField::Port => {
            if let Ok(p) = val.parse() {
                h.port = p;
            }
        }
        EditField::User => h.user = val.to_string(),
        EditField::Group => {
            h.group = if val.is_empty() {
                None
            } else {
                Some(val.to_string())
            }
        }
        EditField::Tags => {
            h.tags = val
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        EditField::KeyPath => {
            h.identity
                .retain(|i| !matches!(i, Identity::Key { .. } | Identity::Agent));
            if !val.is_empty() {
                h.identity.insert(
                    0,
                    Identity::Key {
                        path: val.to_string(),
                        passphrase: None,
                    },
                );
            }
        }
        EditField::JumpHost => {
            h.jump = if val.is_empty() {
                None
            } else {
                Some(val.to_string())
            }
        }
        EditField::Password => {
            h.identity
                .retain(|i| !matches!(i, Identity::Password { .. }));
            if val.eq_ignore_ascii_case("y") || val.eq_ignore_ascii_case("yes") || !val.is_empty() {
                h.identity.push(Identity::Password { from_agent: None });
            }
        }
        EditField::Keepalive => {
            h.keepalive = val.parse().ok();
        }
        EditField::OnConnect => {
            h.on_connect = val
                .split(';')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        EditField::Forwards => {
            h.forwarding = parse_forwards(val);
        }
    }
}

fn parse_forwards(s: &str) -> Vec<PortForward> {
    s.split('|')
        .filter(|s| !s.is_empty())
        .filter_map(|part| {
            let parts: Vec<&str> = part.split(':').collect();
            if parts.len() < 3 {
                return None;
            }
            let dir = match parts[0] {
                "L" => ForwardDirection::Local,
                "R" => ForwardDirection::Remote,
                "D" => ForwardDirection::Dynamic,
                _ => return None,
            };
            let listen_addr = parts[1].to_string();
            let listen_port: u16 = parts[2].parse().ok()?;
            let (target_host, target_port) = if dir == ForwardDirection::Dynamic {
                (String::new(), 0)
            } else if parts.len() >= 5 {
                (parts[3].to_string(), parts[4].parse().ok()?)
            } else if parts.len() == 4 {
                (parts[3].to_string(), 0)
            } else {
                return None;
            };
            static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(100);
            let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(PortForward {
                id,
                direction: dir,
                listen_addr,
                listen_port,
                target_host,
                target_port,
                active: false,
            })
        })
        .collect()
}

fn next_field(f: &EditField) -> Option<EditField> {
    let order = [
        EditField::Name,
        EditField::Host,
        EditField::Port,
        EditField::User,
        EditField::KeyPath,
        EditField::Password,
        EditField::JumpHost,
        EditField::Group,
        EditField::Tags,
        EditField::Keepalive,
        EditField::OnConnect,
        EditField::Forwards,
    ];
    let i = order
        .iter()
        .position(|x| std::mem::discriminant(x) == std::mem::discriminant(f))?;
    order.get(i + 1).cloned()
}

fn parse_user_host(s: &str) -> Option<(String, String, u16)> {
    if let Some(at) = s.find('@') {
        let user = s[..at].to_string();
        let rest = s[at + 1..].to_string();
        if !user.is_empty() && !rest.is_empty() {
            let (host, port) = parse_host_port(&rest);
            return Some((user, host, port));
        }
    }
    if !s.is_empty() {
        let (host, port) = parse_host_port(s);
        return Some(("root".into(), host, port));
    }
    None
}

fn parse_host_port(s: &str) -> (String, u16) {
    if let Some(colon) = s.rfind(':') {
        if let Ok(p) = s[colon + 1..].parse::<u16>() {
            return (s[..colon].to_string(), p);
        }
    }
    (s.to_string(), 22)
}

fn build_opts(h: &Host) -> ConnectOpts {
    let auth: Vec<AuthChoice> = h
        .identity
        .iter()
        .filter_map(|i| match i {
            Identity::Key { path, passphrase } => {
                if path.is_empty() || path == "(none)" {
                    None
                } else {
                    Some(AuthChoice::KeyFile {
                        path: path.clone(),
                        passphrase: passphrase.clone(),
                    })
                }
            }
            Identity::Password { .. } => None,
            Identity::Agent => None,
        })
        .collect();
    ConnectOpts {
        host: h.host.clone(),
        port: h.port,
        user: if h.user.is_empty() {
            "root".into()
        } else {
            h.user.clone()
        },
        auth,
        term_cols: 80,
        term_rows: 24,
        term_type: "xterm-256color".into(),
        keepalive_secs: h.keepalive,
        jump: Vec::new(),
        use_agent: h.identity.iter().any(|i| matches!(i, Identity::Agent)),
    }
}

fn cleanup_forwards(s: &mut Session) {
    if s.forwards.is_empty() && s.remote_forwards.is_none() {
        return;
    }

    if let Some(rf) = &s.remote_forwards {
        let mut map = match rf.try_lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        map.clear();
    }
    s.forwards.clear();
}

fn key_event_to_string(k: &KeyEvent) -> String {
    let prefix = if k.modifiers.contains(KeyModifiers::CONTROL) {
        "ctrl+"
    } else if k.modifiers.contains(KeyModifiers::ALT) {
        "alt+"
    } else if k.modifiers.contains(KeyModifiers::SHIFT) {
        "shift+"
    } else {
        ""
    };
    match k.code {
        KeyCode::Char(c) => format!("{}{}", prefix, c),
        KeyCode::F(n) => format!("{}f{}", prefix, n),
        KeyCode::Enter => format!("{}enter", prefix),
        KeyCode::Esc => format!("{}esc", prefix),
        KeyCode::Tab => format!("{}tab", prefix),
        KeyCode::BackTab => format!("{}backtab", prefix),
        KeyCode::Backspace => format!("{}backspace", prefix),
        KeyCode::Up => format!("{}up", prefix),
        KeyCode::Down => format!("{}down", prefix),
        KeyCode::Left => format!("{}left", prefix),
        KeyCode::Right => format!("{}right", prefix),
        KeyCode::Home => format!("{}home", prefix),
        KeyCode::End => format!("{}end", prefix),
        KeyCode::PageUp => format!("{}pageup", prefix),
        KeyCode::PageDown => format!("{}pagedown", prefix),
        KeyCode::Delete => format!("{}delete", prefix),
        KeyCode::Insert => format!("{}insert", prefix),
        _ => String::new(),
    }
}

fn key_to_bytes(k: KeyEvent) -> Vec<u8> {
    match (k.code, k.modifiers) {
        (KeyCode::Char(c), KeyModifiers::CONTROL) => {
            if c.is_ascii_alphabetic() {
                vec![(c.to_ascii_lowercase() as u8) - b'a' + 1]
            } else if c == '@' || c == ' ' {
                vec![0]
            } else {
                vec![]
            }
        }
        (KeyCode::Char(c), KeyModifiers::ALT) => {
            vec![0x1b, c as u8]
        }
        (KeyCode::Char(c), _) => c.encode_utf8(&mut [0u8; 4]).as_bytes().to_vec(),
        (KeyCode::Enter, _) => b"\r".to_vec(),
        (KeyCode::Backspace, _) => b"\x7f".to_vec(),
        (KeyCode::Tab, _) => b"\t".to_vec(),
        (KeyCode::Esc, _) => b"\x1b".to_vec(),

        (KeyCode::Up, KeyModifiers::NONE) => b"\x1bOA".to_vec(),
        (KeyCode::Down, KeyModifiers::NONE) => b"\x1bOB".to_vec(),
        (KeyCode::Right, KeyModifiers::NONE) => b"\x1bOC".to_vec(),
        (KeyCode::Left, KeyModifiers::NONE) => b"\x1bOD".to_vec(),

        (KeyCode::Up, KeyModifiers::SHIFT) => b"\x1b[1;2A".to_vec(),
        (KeyCode::Down, KeyModifiers::SHIFT) => b"\x1b[1;2B".to_vec(),
        (KeyCode::Right, KeyModifiers::SHIFT) => b"\x1b[1;2C".to_vec(),
        (KeyCode::Left, KeyModifiers::SHIFT) => b"\x1b[1;2D".to_vec(),

        (KeyCode::Up, KeyModifiers::CONTROL) => b"\x1b[1;5A".to_vec(),
        (KeyCode::Down, KeyModifiers::CONTROL) => b"\x1b[1;5B".to_vec(),
        (KeyCode::Right, KeyModifiers::CONTROL) => b"\x1b[1;5C".to_vec(),
        (KeyCode::Left, KeyModifiers::CONTROL) => b"\x1b[1;5D".to_vec(),
        (KeyCode::Home, _) => b"\x1b[H".to_vec(),
        (KeyCode::End, _) => b"\x1b[F".to_vec(),
        (KeyCode::Delete, _) => b"\x1b[3~".to_vec(),
        (KeyCode::Insert, _) => b"\x1b[2~".to_vec(),
        (KeyCode::PageUp, _) => b"\x1b[5~".to_vec(),
        (KeyCode::PageDown, _) => b"\x1b[6~".to_vec(),

        (KeyCode::Up, _) => b"\x1bOA".to_vec(),
        (KeyCode::Down, _) => b"\x1bOB".to_vec(),
        (KeyCode::Right, _) => b"\x1bOC".to_vec(),
        (KeyCode::Left, _) => b"\x1bOD".to_vec(),

        (KeyCode::BackTab, _) => b"\x1b[Z".to_vec(),
        (KeyCode::F(n), _) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5..=12 => format!("\x1b[{}~", n + 10).into_bytes(),
            _ => vec![],
        },
        _ => vec![],
    }
}

#[derive(Debug)]
pub enum Cmd {
    Connect { host: Box<Host> },
    CancelConnect,
    OpenSftp { session_id: SessionId },
    SftpEnter,
    SftpUpDir,
    SftpUpload,
    SftpDownload,
    SftpDelete,
    PortForwardStart { session_id: SessionId, fw_id: u64 },
    PortForwardStop { session_id: SessionId, fw_id: u64 },
}
