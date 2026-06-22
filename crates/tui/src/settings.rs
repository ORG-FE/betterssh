use crate::theme::{self, Theme};
use betterssh_core::Settings;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct SettingsField {
    pub label: &'static str,
    pub value: String,
    pub kind: FieldKind,
    pub key: String,
    pub desc: &'static str,
}

#[derive(Debug, Clone)]
pub enum FieldKind {
    Text,
    Number,
    Bool,
    Select(Vec<String>),
    Label,
}

#[derive(Debug, Clone)]
pub struct SettingsSection {
    pub title: &'static str,
    pub fields: Vec<SettingsField>,
}

#[derive(Debug, Clone)]
pub struct SettingsFocus {
    pub cursor: usize,
    pub editing: bool,
    pub edit_buf: String,
    pub sections: Vec<SettingsSection>,
    pub modified: bool,
    pub scroll_offset: usize,
}

impl SettingsFocus {
    pub fn new(settings: &Settings) -> Self {
        let all_themes = theme::list_theme_names();
        let sections = vec![
            SettingsSection {
                title: "General",
                fields: vec![
                    SettingsField {
                        label: "Default user",
                        value: settings.default_user.clone(),
                        kind: FieldKind::Text,
                        key: "default_user".into(),
                        desc: "username for new hosts",
                    },
                    SettingsField {
                        label: "Keepalive (sec)",
                        value: settings.keepalive_secs.to_string(),
                        kind: FieldKind::Number,
                        key: "keepalive_secs".into(),
                        desc: "SSH keepalive interval",
                    },
                    SettingsField {
                        label: "Terminal type",
                        value: settings.term_type.clone(),
                        kind: FieldKind::Text,
                        key: "term_type".into(),
                        desc: "TERM env on remote",
                    },
                    SettingsField {
                        label: "Scrollback lines",
                        value: settings.scrollback.to_string(),
                        kind: FieldKind::Number,
                        key: "scrollback".into(),
                        desc: "terminal scrollback buffer",
                    },
                    SettingsField {
                        label: "Log lines",
                        value: settings.log_lines.to_string(),
                        kind: FieldKind::Number,
                        key: "log_lines".into(),
                        desc: "event log size",
                    },
                ],
            },
            SettingsSection {
                title: "Appearance",
                fields: vec![
                    SettingsField {
                        label: "Theme",
                        value: settings.theme.clone(),
                        kind: FieldKind::Select(all_themes),
                        key: "theme".into(),
                        desc: "color theme (live reload)",
                    },
                ],
            },
            SettingsSection {
                title: "Behaviour",
                fields: vec![
                    SettingsField {
                        label: "Ping on startup",
                        value: if settings.ping_check { "yes".into() } else { "no".into() },
                        kind: FieldKind::Bool,
                        key: "ping_check".into(),
                        desc: "check host reachability",
                    },
                    SettingsField {
                        label: "Auto reconnect",
                        value: if settings.auto_reconnect { "yes".into() } else { "no".into() },
                        kind: FieldKind::Bool,
                        key: "auto_reconnect".into(),
                        desc: "reconnect on disconnect",
                    },
                    SettingsField {
                        label: "Mouse forwarding",
                        value: if settings.mouse { "yes".into() } else { "no".into() },
                        kind: FieldKind::Bool,
                        key: "mouse".into(),
                        desc: "enable mouse in terminal",
                    },
                    SettingsField {
                        label: "Show metrics",
                        value: if settings.show_metrics { "yes".into() } else { "no".into() },
                        kind: FieldKind::Bool,
                        key: "show_metrics".into(),
                        desc: "show CPU/RAM/disk bar",
                    },
                ],
            },
            SettingsSection {
                title: "Keybindings",
                fields: build_keybinding_fields(settings),
            },
            SettingsSection {
                title: "Macros",
                fields: build_macro_fields(settings),
            },
            SettingsSection {
                title: "About",
                fields: vec![
                    SettingsField {
                        label: "",
                        value: "Made with support from project-fe.dev".into(),
                        kind: FieldKind::Label,
                        key: String::new(),
                        desc: "",
                    },
                    SettingsField {
                        label: "",
                        value: "Main developer: c0redev".into(),
                        kind: FieldKind::Label,
                        key: String::new(),
                        desc: "",
                    },
                    SettingsField {
                        label: "",
                        value: "Website: unitdev.run".into(),
                        kind: FieldKind::Label,
                        key: String::new(),
                        desc: "",
                    },
                ],
            },
        ];
        Self {
            cursor: 0,
            editing: false,
            edit_buf: String::new(),
            sections,
            modified: false,
            scroll_offset: 0,
        }
    }

    fn total_fields(&self) -> usize {
        self.sections.iter().map(|s| s.fields.len()).sum()
    }

    fn section_start(&self, target_section: usize) -> usize {
        let mut idx = 0;
        for (si, sec) in self.sections.iter().enumerate() {
            if si == target_section { return idx; }
            idx += sec.fields.len();
        }
        idx
    }

    fn current_section(&self) -> usize {
        let mut remaining = self.cursor;
        for (si, sec) in self.sections.iter().enumerate() {
            if remaining < sec.fields.len() {
                return si;
            }
            remaining -= sec.fields.len();
        }
        0
    }

    pub fn apply(&self, settings: &mut Settings) {
        for sec in &self.sections {
            for f in &sec.fields {
                if f.key == "default_user" { settings.default_user = f.value.clone(); }
                else if f.key == "keepalive_secs" { settings.keepalive_secs = f.value.parse().unwrap_or(30); }
                else if f.key == "term_type" { settings.term_type = f.value.clone(); }
                else if f.key == "scrollback" { settings.scrollback = f.value.parse().unwrap_or(5000); }
                else if f.key == "log_lines" { settings.log_lines = f.value.parse().unwrap_or(1000); }
                else if f.key == "theme" { settings.theme = f.value.clone(); }
                else if f.key == "ping_check" { settings.ping_check = f.value == "yes"; }
                else if f.key == "auto_reconnect" { settings.auto_reconnect = f.value == "yes"; }
                else if f.key == "mouse" { settings.mouse = f.value == "yes"; }
                else if f.key == "show_metrics" { settings.show_metrics = f.value == "yes"; }
            }
        }
    }

    fn toggle_bool(&mut self, idx: usize) {
        let mut i = 0;
        for sec in &mut self.sections {
            for f in &mut sec.fields {
                if i == idx {
                    if matches!(f.kind, FieldKind::Bool) {
                        f.value = if f.value == "yes" { "no".into() } else { "yes".into() };
                        self.modified = true;
                    }
                    return;
                }
                i += 1;
            }
        }
    }

    fn start_edit(&mut self, idx: usize) {
        let mut i = 0;
        for sec in &self.sections {
            for f in &sec.fields {
                if i == idx {
                    if matches!(f.kind, FieldKind::Text | FieldKind::Number) {
                        self.editing = true;
                        self.edit_buf = f.value.clone();
                    }
                    return;
                }
                i += 1;
            }
        }
    }

    fn select_next_option(&mut self, idx: usize, dir: i32) {
        let mut i = 0;
        for sec in &mut self.sections {
            for f in &mut sec.fields {
                if i == idx {
                    if let FieldKind::Select(ref options) = f.kind {
                        if let Some(pos) = options.iter().position(|o| o == &f.value) {
                            let next = ((pos as i32 + dir).rem_euclid(options.len() as i32)) as usize;
                            f.value = options[next].clone();
                            self.modified = true;
                        } else if !options.is_empty() {
                            f.value = options[0].clone();
                            self.modified = true;
                        }
                    }
                    return;
                }
                i += 1;
            }
        }
    }

    fn cursor_row(&self) -> usize {
        let mut row = 0;
        let mut remaining = self.cursor;
        for sec in &self.sections {
            if remaining < sec.fields.len() {
                row += 2 + remaining;
                return row;
            }
            row += 2 + sec.fields.len() + 1;
            remaining -= sec.fields.len();
        }
        row
    }

    fn cycle_field(&mut self, idx: usize, dir: i32) {
        let total = self.total_fields();
        let next = ((idx as i32 + dir).rem_euclid(total as i32)) as usize;
        self.cursor = next;
    }

    fn jump_section(&mut self, dir: i32) {
        let cur = self.current_section();
        let n = self.sections.len();
        let next = ((cur as i32 + dir).rem_euclid(n as i32)) as usize;
        self.cursor = self.section_start(next);
    }

    pub fn handle_key(&mut self, k: KeyEvent) -> Option<SettingsAction> {
        if self.editing {
            match k.code {
                KeyCode::Enter => {
                    self.commit_edit();
                    self.editing = false;
                    self.modified = true;
                }
                KeyCode::Esc => {
                    self.editing = false;
                }
                KeyCode::Char(c) => {
                    self.edit_buf.push(c);
                }
                KeyCode::Backspace => {
                    self.edit_buf.pop();
                }
                _ => {}
            }
            return None;
        }

        match k.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.cycle_field(self.cursor, -1);
                
                if self.cursor_row() < self.scroll_offset {
                    self.scroll_offset = self.cursor_row();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cycle_field(self.cursor, 1);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.select_next_option(self.cursor, 1);
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.select_next_option(self.cursor, -1);
            }
            KeyCode::Tab if !k.modifiers.contains(KeyModifiers::SHIFT) => {
                self.jump_section(1);
            }
            KeyCode::BackTab | KeyCode::Tab if k.modifiers.contains(KeyModifiers::SHIFT) => {
                self.jump_section(-1);
            }
            KeyCode::PageDown => {
                self.cycle_field(self.cursor, 5);
                self.scroll_offset = self.scroll_offset.saturating_add(5);
            }
            KeyCode::PageUp => {
                self.cycle_field(self.cursor, -5);
                self.scroll_offset = self.scroll_offset.saturating_sub(5);
            }
            KeyCode::Home => {
                self.cursor = 0;
                self.scroll_offset = 0;
            }
            KeyCode::End => {
                self.cursor = self.total_fields().saturating_sub(1);
                self.scroll_offset = usize::MAX; 
            }
            KeyCode::Enter => {
                if let Some(f) = self.field_at(self.cursor) {
                    match f.kind {
                        FieldKind::Bool => self.toggle_bool(self.cursor),
                        FieldKind::Select(_) => self.select_next_option(self.cursor, 1),
                        FieldKind::Text | FieldKind::Number => self.start_edit(self.cursor),
                        FieldKind::Label => {
                            if f.key == "kb_add" {
                                return Some(SettingsAction::AddKeybinding);
                            } else if let Some(idx_str) = f.key.strip_prefix("kb_") {
                                if let Ok(idx) = idx_str.parse::<usize>() {
                                    return Some(SettingsAction::EditKeybinding { idx });
                                }
                            } else if f.key == "macro_add" {
                                return Some(SettingsAction::AddMacro);
                            } else if let Some(idx_str) = f.key.strip_prefix("macro_") {
                                if let Ok(idx) = idx_str.parse::<usize>() {
                                    return Some(SettingsAction::EditMacro { idx });
                                }
                            }
                        }
                    }
                }
            }
            KeyCode::Char('s') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                return Some(SettingsAction::Save);
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.modified {
                    return Some(SettingsAction::ConfirmDiscard);
                }
                return Some(SettingsAction::Close);
            }
            _ => {}
        }
        None
    }

    fn field_at(&self, idx: usize) -> Option<&SettingsField> {
        let mut i = 0;
        for sec in &self.sections {
            for f in &sec.fields {
                if i == idx { return Some(f); }
                i += 1;
            }
        }
        None
    }

    fn commit_edit(&mut self) {
        let mut i = 0;
        for sec in &mut self.sections {
            for f in &mut sec.fields {
                if i == self.cursor {
                    f.value = self.edit_buf.clone();
                    return;
                }
                i += 1;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingsAction {
    Save,
    Close,
    ConfirmDiscard,
    EditKeybinding { idx: usize },
    AddKeybinding,
    EditMacro { idx: usize },
    AddMacro,
}

pub fn draw_settings(f: &mut Frame, area: Rect, s: &mut SettingsFocus, theme: &Theme, confirm_discard: bool) {
    let popup = centered_rect(area, 62, 72);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(" ⚙ Settings ")
        .title_alignment(Alignment::Center)
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(popup);

    f.render_widget(Clear, popup);
    f.render_widget(&block, popup);

    let vchunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let body_area = vchunks[0];
    let footer_area = vchunks[1];

    let mut rows: Vec<Line> = Vec::new();
    let mut idx = 0;
    let cursor_sec = s.current_section();

    for (si, sec) in s.sections.iter().enumerate() {
        let is_active_section = si == cursor_sec;
        let sec_color = if is_active_section { theme.accent } else { theme.dim };
        let is_about = sec.title == "About";

        if is_about {
            
            let phase = animation_phase(3000);
            let nick_color = cycle_color(theme.accent2, theme.accent, theme.good, phase);

            let raw_lines = vec![
                format!("Made with {} by project-fe.dev", '\u{2665}'),
                "Main developer: c0redev".into(),
                "Website: unitdev.run".into(),
            ];
            let max_w = raw_lines.iter().map(|l| l.len()).max().unwrap_or(0);
            let pad = (body_area.width as usize).saturating_sub(max_w) / 2;

            rows.push(Line::from(""));
            rows.push(Line::from(vec![
                Span::styled(" ".repeat(pad), Style::default()),
                Span::styled("About", Style::default().fg(sec_color).add_modifier(Modifier::BOLD)),
            ]));
            rows.push(Line::from(""));

            rows.push(Line::from(vec![
                Span::styled(" ".repeat(pad), Style::default()),
                Span::styled("Made with ", Style::default().fg(theme.dim)),
                Span::styled("\u{2665}", Style::default().fg(theme.warn)),
                Span::styled(" by ", Style::default().fg(theme.dim)),
                Span::styled("project-fe.dev", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
            ]));
            rows.push(Line::from(""));
            rows.push(Line::from(vec![
                Span::styled(" ".repeat(pad), Style::default()),
                Span::styled("Main developer: ", Style::default().fg(theme.dim)),
                Span::styled("c0redev", Style::default().fg(nick_color).add_modifier(Modifier::BOLD)),
            ]));
            rows.push(Line::from(vec![
                Span::styled(" ".repeat(pad), Style::default()),
                Span::styled("Website: ", Style::default().fg(theme.dim)),
                Span::styled("unitdev.run", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
            ]));
            rows.push(Line::from(""));
            continue;
        }

        rows.push(Line::from(vec![
            Span::styled(" ", Style::default().fg(theme.bg)),
            Span::styled(
                format!(" {} ", sec.title),
                Style::default().fg(sec_color).add_modifier(Modifier::BOLD),
            ),
        ]));
        rows.push(Line::from(""));

        for f in &sec.fields {
            let selected = idx == s.cursor;
            let editing = selected && s.editing;

            let indicator = match f.kind {
                FieldKind::Text | FieldKind::Number => "▸",
                FieldKind::Bool => if f.value == "yes" { "✓" } else { "✗" },
                FieldKind::Select(_) => "►",
                _ => "",
            };
            let indicator_color = match f.kind {
                FieldKind::Bool if f.value == "yes" => theme.good,
                FieldKind::Bool => theme.bad,
                _ => theme.accent,
            };

            let label_w = sec.fields.iter().map(|ff| ff.label.len()).max().unwrap_or(20);
            let padding = " ".repeat(label_w.saturating_sub(f.label.len()) + 1);

            let val = if editing {
                format!(" {}_", s.edit_buf)
            } else {
                format!("{}", f.value)
            };

            let val_color = if editing {
                theme.accent
            } else {
                match f.kind {
                    FieldKind::Bool if f.value == "yes" => theme.good,
                    FieldKind::Bool => theme.bad,
                    _ => theme.txt,
                }
            };

            let line_spans = vec![
                Span::styled("  ", Style::default()),
                Span::styled(indicator, Style::default().fg(indicator_color)),
                Span::styled(" ", Style::default()),
                Span::styled(f.label, Style::default().fg(theme.txt)),
                Span::styled(padding, Style::default()),
                Span::styled(val, Style::default().fg(val_color)),
            ];

            if selected {
                let mut row_style = Style::default().bg(theme.sel_bg);
                if editing {
                    row_style = row_style.add_modifier(Modifier::SLOW_BLINK);
                }
                let highlighted: Vec<Span> = line_spans.into_iter()
                    .map(|sp| Span::styled(sp.content.clone(), sp.style.patch(row_style)))
                    .collect();
                rows.push(Line::from(highlighted));
            } else {
                rows.push(Line::from(line_spans));
            }
            idx += 1;
        }
        rows.push(Line::from(""));
    }

    
    let theme_idx = s.field_index_by_key("theme");
    if let Some(ti) = theme_idx {
        if cursor_sec == s.section_of(ti) {
            let preview_theme = theme::load_theme(&s.field_value_by_key("theme").unwrap_or_default());
            let swatches = theme_swatches(&preview_theme);
            for swatch_line in swatches {
                rows.push(swatch_line);
            }
            rows.push(Line::from(""));
        }
    }

    let visible = body_area.height as usize;
    
    let cursor_row = s.cursor_row();
    
    if s.sections.get(cursor_sec).is_some_and(|sec| sec.title == "About") {
        s.scroll_offset = rows.len().saturating_sub(visible);
    } else {
        if cursor_row >= s.scroll_offset + visible {
            s.scroll_offset = cursor_row.saturating_sub(visible).saturating_add(1);
        }
        if cursor_row < s.scroll_offset {
            s.scroll_offset = cursor_row;
        }
    }
    s.scroll_offset = s.scroll_offset.min(rows.len().saturating_sub(visible));

    let p = Paragraph::new(rows)
        .scroll((s.scroll_offset as u16, 0))
        .style(Style::default().bg(theme.bg));
    f.render_widget(p, body_area);

    
    let footer = if confirm_discard {
        Line::from(vec![
            Span::styled(" Discard changes? ", Style::default().fg(theme.warn).add_modifier(Modifier::BOLD)),
            Span::styled("y", Style::default().fg(theme.good)),
            Span::styled("/", Style::default().fg(theme.dim)),
            Span::styled("N", Style::default().fg(theme.bad)),
            Span::styled("  ", Style::default()),
            Span::styled("any other key", Style::default().fg(theme.dim)),
            Span::styled(" = cancel", Style::default().fg(theme.dim)),
        ])
    } else {
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("↑↓", Style::default().fg(theme.good)),
            Span::styled(" nav  ", Style::default().fg(theme.dim)),
            Span::styled("Tab", Style::default().fg(theme.good)),
            Span::styled(" sect  ", Style::default().fg(theme.dim)),
            Span::styled("Enter", Style::default().fg(theme.good)),
            Span::styled(" edit  ", Style::default().fg(theme.dim)),
            Span::styled("←→", Style::default().fg(theme.good)),
            Span::styled(" cycle  ", Style::default().fg(theme.dim)),
            Span::styled("^S", Style::default().fg(theme.accent)),
            Span::styled(" save  ", Style::default().fg(theme.dim)),
            Span::styled("Esc", Style::default().fg(theme.bad)),
            Span::styled(" close", Style::default().fg(theme.dim)),
        ])
    };
    f.render_widget(Paragraph::new(footer).style(Style::default().bg(theme.bg)), footer_area);
}

fn theme_swatches(t: &Theme) -> Vec<Line<'static>> {
    let pairs = [
        ("bg", t.bg), ("pnl", t.panel), ("p2", t.panel2),
        ("bdr", t.border), ("bd2", t.border2),
        ("txt", t.txt), ("dim", t.dim),
        ("acc", t.accent), ("ac2", t.accent2),
        ("ok", t.good), ("warn", t.warn), ("bad", t.bad),
        ("sb", t.sel_bg), ("sf", t.sel_fg),
        ("surf", t.surface), ("ovl", t.overlay),
    ];

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("  ── Theme Preview ──", Style::default().fg(t.dim)),
    ]));

    for chunk in pairs.chunks(8) {
        let mut spans = vec![Span::styled("  ", Style::default())];
        for (label, color) in chunk {
            let text_color = if is_dark(*color) { t.txt } else { t.bg };
            spans.push(Span::styled("██", Style::default().bg(*color).fg(text_color)));
            spans.push(Span::styled(
                format!(" {} ", label),
                Style::default().fg(t.dim),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn is_dark(c: ratatui::style::Color) -> bool {
    if let ratatui::style::Color::Rgb(r, g, b) = c {
        let lum = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
        lum < 128.0
    } else {
        true
    }
}

fn centered_rect(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let w = area.width.saturating_mul(pct_x.min(100)).saturating_div(100).max(44);
    let h = area.height.saturating_mul(pct_y.min(100)).saturating_div(100).max(12);
    let x = area.width.saturating_sub(w).saturating_div(2);
    let y = area.height.saturating_sub(h).saturating_div(2);
    Rect { x: x.saturating_add(area.x), y: y.saturating_add(area.y), width: w, height: h }
}

fn animation_phase(period_ms: u64) -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    (now % period_ms) as f64 / period_ms as f64
}

fn lerp_color(a: Color, b: Color, t: f64) -> Color {
    let (ar, ag, ab) = rgb_parts(a);
    let (br, bg, bb) = rgb_parts(b);
    let t = t.clamp(0.0, 1.0);
    Color::Rgb(
        (ar as f64 + (br as f64 - ar as f64) * t) as u8,
        (ag as f64 + (bg as f64 - ag as f64) * t) as u8,
        (ab as f64 + (bb as f64 - ab as f64) * t) as u8,
    )
}

fn cycle_color(a: Color, b: Color, c: Color, phase: f64) -> Color {
    
    let p = phase * 3.0; 
    if p < 1.0 {
        lerp_color(a, b, p)
    } else if p < 2.0 {
        lerp_color(b, c, p - 1.0)
    } else {
        lerp_color(c, a, p - 2.0)
    }
}

fn rgb_parts(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

fn build_keybinding_fields(settings: &Settings) -> Vec<SettingsField> {
    let mut fields = Vec::new();
    let mut entries: Vec<(String, String)> = settings.keybindings.iter()
        .map(|(k, v)| (k.clone(), v.clone())).collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (i, (action, combo)) in entries.iter().enumerate() {
        fields.push(SettingsField {
            label: "",
            value: format!(" {} = {}", action, combo),
            kind: FieldKind::Label,
            key: format!("kb_{}", i),
            desc: "",
        });
    }
    fields.push(SettingsField {
        label: "",
        value: "[add binding]".into(),
        kind: FieldKind::Label,
        key: "kb_add".into(),
        desc: "",
    });
    fields
}

fn build_macro_fields(settings: &Settings) -> Vec<SettingsField> {
    let mut fields = Vec::new();
    for (i, m) in settings.macros.iter().enumerate() {
        let cmds = m.commands.join("; ");
        fields.push(SettingsField {
            label: "",
            value: format!(" {}: {}", m.name, cmds),
            kind: FieldKind::Label,
            key: format!("macro_{}", i),
            desc: "",
        });
    }
    fields.push(SettingsField {
        label: "",
        value: "[add macro]".into(),
        kind: FieldKind::Label,
        key: "macro_add".into(),
        desc: "",
    });
    fields
}

impl SettingsFocus {
    pub fn rebuild_section(&mut self, title: &str, settings: &Settings) {
        for sec in &mut self.sections {
            if sec.title != title { continue; }
            sec.fields = match title {
                "Keybindings" => build_keybinding_fields(settings),
                "Macros" => build_macro_fields(settings),
                _ => return,
            };
            return;
        }
    }

    fn field_index_by_key(&self, key: &str) -> Option<usize> {
        let mut i = 0;
        for sec in &self.sections {
            for f in &sec.fields {
                if f.key == key { return Some(i); }
                i += 1;
            }
        }
        None
    }

    fn section_of(&self, field_idx: usize) -> usize {
        let mut rem = field_idx;
        for (si, sec) in self.sections.iter().enumerate() {
            if rem < sec.fields.len() { return si; }
            rem -= sec.fields.len();
        }
        0
    }

    fn field_value_by_key(&self, key: &str) -> Option<String> {
        for sec in &self.sections {
            for f in &sec.fields {
                if f.key == key { return Some(f.value.clone()); }
            }
        }
        None
    }
}
