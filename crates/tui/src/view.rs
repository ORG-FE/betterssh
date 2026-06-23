use crate::state::{HostStatus, MsgLevel, SftpPane, SftpState, Toast};
use crate::theme::Theme;
use betterssh_core::{Host, Snippet};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap,
};
use ratatui::Frame;
use std::collections::{HashMap, HashSet};

#[allow(clippy::too_many_arguments)]
pub fn draw_host_list(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    hosts: &[Host],
    state: &mut ListState,
    query: &str,
    focused: bool,
    host_status: &HashMap<String, HostStatus>,
    group_mode: bool,
    collapsed_groups: &HashSet<String>,
    batch_selected: &HashSet<String>,
) {
    let mut filtered: Vec<&Host> = if query.is_empty() {
        hosts.iter().collect()
    } else {
        let q = query.to_lowercase();
        hosts
            .iter()
            .filter(|h| {
                h.name.to_lowercase().contains(&q)
                    || h.host.to_lowercase().contains(&q)
                    || h.user.to_lowercase().contains(&q)
                    || h.group
                        .as_deref()
                        .map(|g| g.to_lowercase().contains(&q))
                        .unwrap_or(false)
                    || h.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .collect()
    };

    let items: Vec<ListItem> = if group_mode {
        filtered.sort_by(|a, b| {
            let ga = a.group.as_deref().unwrap_or("");
            let gb = b.group.as_deref().unwrap_or("");
            ga.cmp(gb).then(a.name.cmp(&b.name))
        });

        let mut list_items: Vec<ListItem> = Vec::new();
        let mut current_group: Option<String> = None;

        for h in &filtered {
            let grp = h.group.as_deref().unwrap_or("ungrouped").to_string();

            if collapsed_groups.contains(&grp) {
                continue;
            }

            if current_group.as_deref() != Some(&grp) {
                let collapsed = collapsed_groups.contains(&grp);
                let g_icon = if collapsed { "\u{25b6}" } else { "\u{25bc}" };
                list_items.push(ListItem::new(vec![Line::from(Span::styled(
                    format!(" {} {} ", g_icon, grp),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ))]));
                current_group = Some(grp);
            }

            let status = host_status.get(&h.name).unwrap_or(&HostStatus::Unknown);
            let (icon, indicator_fg) = match status {
                HostStatus::Alive => ("\u{25cf}", theme.good),
                HostStatus::Dead(_) => ("\u{2715}", theme.bad),
                HostStatus::Unknown => ("\u{25cb}", theme.dim),
            };
            let checked = if batch_selected.contains(&h.name) {
                "\u{2611} "
            } else {
                "\u{2610} "
            };
            list_items.push(ListItem::new(Line::from(vec![
                Span::styled(format!(" {}", checked), Style::default().fg(theme.accent)),
                Span::styled(format!(" {} ", icon), Style::default().fg(indicator_fg)),
                Span::styled(
                    format!(" {}", truncate(&h.name, 18)),
                    Style::default().fg(theme.txt),
                ),
                Span::styled(
                    format!(" {}@{}", h.user, h.addr()),
                    Style::default().fg(theme.dim),
                ),
            ])));
        }
        list_items
    } else {
        filtered
            .iter()
            .map(|h| {
                let status = host_status.get(&h.name).unwrap_or(&HostStatus::Unknown);
                let (icon, indicator_fg) = match status {
                    HostStatus::Alive => ("\u{25cf}", theme.good),
                    HostStatus::Dead(_) => ("\u{2715}", theme.bad),
                    HostStatus::Unknown => ("\u{25cb}", theme.dim),
                };
                let checked = if batch_selected.contains(&h.name) {
                    "\u{2611} "
                } else {
                    "\u{2610} "
                };
                ListItem::new(Line::from(vec![
                    Span::styled(checked, Style::default().fg(theme.accent)),
                    Span::styled(format!(" {} ", icon), Style::default().fg(indicator_fg)),
                    Span::styled(
                        format!(" {}", truncate(&h.name, 18)),
                        Style::default().fg(theme.txt),
                    ),
                    Span::styled(
                        format!(" {}@{}", h.user, h.addr()),
                        Style::default().fg(theme.dim),
                    ),
                ]))
            })
            .collect()
    };

    let border_color = if focused { theme.accent } else { theme.border2 };
    let title = if query.is_empty() {
        format!(" HOSTS ({}) ", items.len())
    } else {
        format!(" HOSTS ({}/{}) /{} ", items.len(), hosts.len(), query)
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border_color))
                .title(Span::styled(
                    title,
                    Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
                ))
                .style(Style::default().bg(theme.panel)),
        )
        .highlight_style(
            Style::default()
                .bg(theme.sel_bg)
                .fg(theme.sel_fg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, state);
}

#[allow(clippy::too_many_arguments)]
pub fn draw_terminal(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    title: &str,
    lines: &[Line<'static>],
    cursor: Option<(u16, u16)>,
    status: &str,
    focused: bool,
) {
    let border_color = if focused { theme.accent } else { theme.border2 };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!(" {} ", title),
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(theme.bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let para = Paragraph::new(lines.to_vec()).wrap(Wrap { trim: false });
    f.render_widget(para, chunks[0]);

    if let Some((x, y)) = cursor {
        if x < chunks[0].width && y < chunks[0].height {
            f.set_cursor_position((chunks[0].x + x, chunks[0].y + y));
        }
    }

    let status_line = Paragraph::new(Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(status, Style::default().fg(theme.dim)),
    ]))
    .style(Style::default().bg(theme.panel2));
    f.render_widget(status_line, chunks[1]);
}

pub fn draw_status_bar(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    hints: &[(&str, &str)],
    msg: Option<&str>,
) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(40)])
        .split(area);

    let mut spans: Vec<Span> = Vec::new();
    for (i, (k, v)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                " \u{2502} ",
                Style::default().fg(theme.border),
            ));
        }
        spans.push(Span::styled(
            format!(" {}", k),
            Style::default().fg(theme.bg).bg(theme.accent2),
        ));
        spans.push(Span::styled(
            format!(" {} ", v),
            Style::default().fg(theme.txt),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.panel)),
        chunks[0],
    );

    let right = match msg {
        Some(m) => Span::styled(m, Style::default().fg(theme.warn)),
        None => Span::styled("\u{25c9} ready", Style::default().fg(theme.dim)),
    };
    f.render_widget(
        Paragraph::new(Line::from(right))
            .alignment(ratatui::layout::Alignment::Right)
            .style(Style::default().bg(theme.panel)),
        chunks[1],
    );
}

pub fn draw_prompt(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    label: &str,
    value: &str,
    cursor: usize,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent2))
        .style(Style::default().bg(theme.surface));
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let label_span = Span::styled(
        format!(" {} ", label),
        Style::default()
            .fg(theme.bg)
            .bg(theme.accent2)
            .add_modifier(Modifier::BOLD),
    );
    let value_span = Span::styled(value.to_string(), Style::default().fg(theme.txt));
    let cursor_x = label.len() + 3 + cursor;

    let line = Line::from(vec![label_span, Span::raw(" "), value_span]);
    f.render_widget(Paragraph::new(line), inner);
    f.set_cursor_position((inner.x + cursor_x as u16, inner.y));
}

pub fn draw_connecting(f: &mut Frame, area: Rect, theme: &Theme, host: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.warn))
        .style(Style::default().bg(theme.panel));
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let text = Line::from(vec![
        Span::styled("Connecting to ", Style::default().fg(theme.dim)),
        Span::styled(
            host,
            Style::default().fg(theme.txt).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ...", Style::default().fg(theme.dim)),
    ]);
    f.render_widget(
        Paragraph::new(text).alignment(ratatui::layout::Alignment::Center),
        inner,
    );
}

pub fn draw_table(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    title: &str,
    headers: &[&str],
    rows: &[Vec<String>],
    widths: &[Constraint],
) {
    let header = Row::new(headers.iter().map(|h| {
        Cell::from(*h).style(Style::default().fg(theme.dim).add_modifier(Modifier::BOLD))
    }))
    .style(Style::default().bg(theme.panel2));

    let body: Vec<Row> = rows
        .iter()
        .map(|r| Row::new(r.iter().map(|c| Cell::from(c.as_str()))))
        .collect();

    let t = Table::new(body, widths.to_vec()).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border2))
            .title(Span::styled(
                format!(" {} ", title),
                Style::default().fg(theme.dim),
            )),
    );
    f.render_widget(t, area);
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

pub fn draw_sftp(f: &mut Frame, area: Rect, theme: &Theme, sftp: &SftpState) {
    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border2))
        .title(Span::styled(
            " SFTP ",
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(theme.bg));
    let inner = outer_block.inner(area);
    f.render_widget(outer_block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);

    draw_sftp_pane(f, cols[0], theme, sftp, SftpPane::Local);
    draw_sftp_pane(f, cols[1], theme, sftp, SftpPane::Remote);
}

fn draw_sftp_pane(f: &mut Frame, area: Rect, theme: &Theme, sftp: &SftpState, pane: SftpPane) {
    let focused = sftp.focus == pane;
    let border_color = if focused {
        theme.accent2
    } else {
        theme.border2
    };
    let path_str = sftp.pane_path(pane).display();
    let pane_icon = match pane {
        SftpPane::Local => "\u{25c9}",
        SftpPane::Remote => "\u{25b6}",
    };
    let title = format!(" {} {} ", pane_icon, path_str);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title,
            Style::default().fg(theme.txt).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(theme.panel));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let header = Row::new(vec![
        Cell::from(" name").style(Style::default().fg(theme.dim)),
        Cell::from("size").style(Style::default().fg(theme.dim)),
    ])
    .style(Style::default().bg(theme.surface));

    let entries: &[crate::state::SftpEntry] = match pane {
        SftpPane::Local => &sftp.local_entries,
        SftpPane::Remote => &sftp.remote_entries,
    };

    let filtered: Vec<&crate::state::SftpEntry> = if sftp.filter.is_empty() {
        entries.iter().collect()
    } else {
        let q = sftp.filter.to_lowercase();
        entries
            .iter()
            .filter(|e| e.name.to_lowercase().contains(&q))
            .collect()
    };

    let rows: Vec<Row> = filtered
        .iter()
        .map(|e| {
            let icon = if e.is_dir { "\u{25b6}" } else { " " };
            let name_display = format!(" {} {}", icon, e.name);
            let size_str = if e.is_dir {
                "-".to_string()
            } else {
                format_size(e.size)
            };
            Row::new(vec![
                Cell::from(name_display),
                Cell::from(size_str).style(Style::default().fg(theme.dim)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        vec![Constraint::Percentage(70), Constraint::Length(10)],
    )
    .header(header)
    .column_spacing(1)
    .style(Style::default().bg(theme.panel))
    .highlight_style(Style::default().bg(theme.sel_bg).fg(theme.sel_fg));

    let sel = sftp.sel.min(filtered.len().saturating_sub(1));
    let mut state = ratatui::widgets::TableState::default();
    state.select(Some(sel));
    f.render_stateful_widget(table, inner, &mut state);
}

fn format_size(n: u64) -> String {
    const UNITS: &[&str] = &["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{}", n)
    } else {
        format!("{:.1}{}", v, UNITS[i])
    }
}

pub fn draw_snippets_bar(f: &mut Frame, area: Rect, theme: &Theme, snippets: &[Snippet]) {
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, s) in snippets.iter().take(9).enumerate() {
        let label = format!(" {} ", i + 1);
        let name = format!(" {} ", s.name);
        spans.push(Span::styled(
            label,
            Style::default().fg(theme.bg).bg(theme.accent),
        ));
        spans.push(Span::styled(name, Style::default().fg(theme.txt)));
    }
    let line = Line::from(spans);
    let p = Paragraph::new(line);
    f.render_widget(p, area);
}

pub fn popup_area(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(v[1]);
    h[1]
}

pub fn draw_toasts(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    toasts: &std::collections::VecDeque<Toast>,
) {
    if toasts.is_empty() {
        return;
    }
    let max_w = (area.width / 3).min(60) as usize;
    let toast_count = toasts.len().min(8) as u16;
    let toast_x = area.x + area.width.saturating_sub(max_w as u16 + 2);
    let toast_area = Rect {
        x: toast_x,
        y: area.y + 1,
        width: max_w as u16 + 2,
        height: toast_count,
    };
    let mut lines: Vec<Line> = Vec::new();
    let avail_w = max_w;
    for t in toasts.iter().take(8) {
        let icon = match t.level {
            MsgLevel::Info => "\u{2139}",
            MsgLevel::Warn => "\u{26a0}",
            MsgLevel::Bad => "\u{2716}",
        };
        let text = if t.text.chars().count() > avail_w {
            let truncated: String = t.text.chars().take(avail_w.saturating_sub(1)).collect();
            format!("{} {}…", icon, truncated)
        } else {
            format!("{} {}", icon, t.text)
        };
        let color = match t.level {
            MsgLevel::Info => theme.accent2,
            MsgLevel::Warn => theme.warn,
            MsgLevel::Bad => theme.bad,
        };
        lines.push(Line::from(Span::styled(
            text,
            Style::default().fg(color).bg(theme.surface),
        )));
    }
    let p = Paragraph::new(lines).style(Style::default().bg(theme.surface));
    f.render_widget(Clear, toast_area);
    f.render_widget(p, toast_area);
}
