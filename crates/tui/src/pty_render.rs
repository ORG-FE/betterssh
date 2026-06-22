use crate::theme::Theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;
use vt100::Parser;

const SCROLLBACK: usize = 2000;

pub struct TerminalView {
    pub parser: Parser,
    cols: u16,
    rows: u16,
    pub scroll_offset: u16,
}

impl TerminalView {
    pub fn new(cols: u16, rows: u16) -> Self {
        let safe_cols = cols.max(2);
        let safe_rows = rows.max(2);
        Self {
            parser: Parser::new(safe_rows, safe_cols, SCROLLBACK),
            cols: safe_cols,
            rows: safe_rows,
            scroll_offset: 0,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(2);
        let rows = rows.max(2);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.parser.screen_mut().set_size(rows, cols);
        self.cols = cols;
        self.rows = rows;
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn cursor(&self) -> (u16, u16) {
        let (r, c) = self.parser.screen().cursor_position();
        (c as u16, r as u16)
    }

    pub fn scroll_up(&mut self, n: u16) {
        let max = self.parser.screen().scrollback() as u16;
        self.scroll_offset = (self.scroll_offset + n).min(max);
    }

    pub fn scroll_down(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    pub fn render(&self, theme: &Theme) -> Vec<Line<'static>> {
        if self.scroll_offset == 0 {
            return self.render_visible(theme);
        }
        self.render_scrollback(theme)
    }

    pub fn raw_lines(&self) -> Vec<String> {
        if self.scroll_offset == 0 {
            return self.raw_visible();
        }
        self.raw_scrollback()
    }

    fn raw_visible(&self) -> Vec<String> {
        let screen = self.parser.screen();
        let mut out = Vec::with_capacity(self.rows as usize);
        for y in 0..self.rows {
            let mut s = String::new();
            for x in 0..self.cols {
                if let Some(c) = screen.cell(y, x) {
                    s.push(c.contents().chars().next().unwrap_or(' '));
                } else {
                    s.push(' ');
                }
            }
            out.push(s);
        }
        out
    }

    fn raw_scrollback(&self) -> Vec<String> {
        let screen = self.parser.screen();
        let total = screen.scrollback() + self.rows as usize;
        let want_from = total.saturating_sub(self.rows as usize + self.scroll_offset as usize);
        let want_to = want_from + self.rows as usize;
        let mut out = Vec::with_capacity(self.rows as usize);
        for y in want_from..want_to.min(total) {
            let mut s = String::new();
            for x in 0..self.cols as u16 {
                if let Some(cell) = screen.cell(y as u16, x) {
                    s.push(cell.contents().chars().next().unwrap_or(' '));
                } else {
                    s.push(' ');
                }
            }
            out.push(s);
        }
        out
    }

    fn render_visible(&self, theme: &Theme) -> Vec<Line<'static>> {
        let screen = self.parser.screen();
        let mut out: Vec<Line> = Vec::with_capacity(self.rows as usize);
        for y in 0..self.rows {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut current_style = Style::default();
            let mut current_text = String::new();

            for x in 0..self.cols {
                let cell = screen.cell(y, x);
                let (ch, fg, bg, bold, reverse) = match cell {
                    Some(c) => (
                        c.contents().chars().next().unwrap_or(' '),
                        c.fgcolor(),
                        c.bgcolor(),
                        c.bold(),
                        c.inverse(),
                    ),
                    None => (' ', vt100::Color::Default, vt100::Color::Default, false, false),
                };

                let w = ch.width().unwrap_or(0);
                let style = to_style(fg, bg, bold, reverse, theme);
                if style != current_style && !current_text.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current_text), current_style));
                }
                current_style = style;
                if w == 0 {
                    continue;
                }
                current_text.push(ch);
            }
            if !current_text.is_empty() {
                spans.push(Span::styled(current_text, current_style));
            }
            out.push(Line::from(spans));
        }
        out
    }

    fn render_scrollback(&self, theme: &Theme) -> Vec<Line<'static>> {
        
        let screen = self.parser.screen();
        let total = screen.scrollback() + self.rows as usize;
        let want_from = total.saturating_sub(self.rows as usize + self.scroll_offset as usize);
        let want_to = want_from + self.rows as usize;

        let mut out: Vec<Line> = Vec::with_capacity(self.rows as usize);
        for y in want_from..want_to.min(total) {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut current_style = Style::default();
            let mut current_text = String::new();

            for x in 0..self.cols {
                let cell = screen.cell(y as u16, x);
                let (ch, fg, bg, bold, reverse) = match cell {
                    Some(c) => (
                        c.contents().chars().next().unwrap_or(' '),
                        c.fgcolor(),
                        c.bgcolor(),
                        c.bold(),
                        c.inverse(),
                    ),
                    None => (' ', vt100::Color::Default, vt100::Color::Default, false, false),
                };

                let w = ch.width().unwrap_or(0);
                let style = to_style(fg, bg, bold, reverse, theme);
                if style != current_style && !current_text.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current_text), current_style));
                }
                current_style = style;
                if w == 0 {
                    continue;
                }
                current_text.push(ch);
            }
            if !current_text.is_empty() {
                spans.push(Span::styled(current_text, current_style));
            }
            out.push(Line::from(spans));
        }
        while out.len() < self.rows as usize {
            out.push(Line::from(""));
        }
        out
    }
}

fn to_style(fg: vt100::Color, bg: vt100::Color, bold: bool, reverse: bool, theme: &Theme) -> Style {
    let fg_c = color_to_ratatui(fg, theme.txt);
    let bg_c = color_to_ratatui(bg, theme.bg);
    let (f, b) = if reverse { (bg_c, fg_c) } else { (fg_c, bg_c) };
    let mut s = Style::default().fg(f).bg(b);
    if bold {
        s = s.add_modifier(Modifier::BOLD);
    }
    s
}

fn color_to_ratatui(c: vt100::Color, def: Color) -> Color {
    match c {
        vt100::Color::Default => def,
        vt100::Color::Idx(i) => ansi_palette(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn ansi_palette(i: u8) -> Color {
    match i {
        0 => Color::Rgb(0x15, 0x18, 0x1b),
        1 => Color::Rgb(0xcd, 0x6a, 0x5a),
        2 => Color::Rgb(0x6f, 0xae, 0x6f),
        3 => Color::Rgb(0xc9, 0xa1, 0x4a),
        4 => Color::Rgb(0x5b, 0x93, 0xb8),
        5 => Color::Rgb(0xb7, 0x6e, 0x9d),
        6 => Color::Rgb(0x6f, 0xb7, 0xb7),
        7 => Color::Rgb(0xc4, 0xc9, 0xce),
        8 => Color::Rgb(0x44, 0x4d, 0x54),
        9 => Color::Rgb(0xe8, 0x7a, 0x6a),
        10 => Color::Rgb(0x8f, 0xc6, 0x8f),
        11 => Color::Rgb(0xe3, 0xc2, 0x6a),
        12 => Color::Rgb(0x7f, 0xb7, 0xd7),
        13 => Color::Rgb(0xd7, 0x9c, 0xbf),
        14 => Color::Rgb(0x9f, 0xd7, 0xd7),
        15 => Color::Rgb(0xe7, 0xea, 0xec),
        _ => Color::Reset,
    }
}
