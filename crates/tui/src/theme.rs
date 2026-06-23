use ratatui::style::Color;

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub bg: Color,
    pub panel: Color,
    pub panel2: Color,
    pub border: Color,
    pub border2: Color,
    pub txt: Color,
    pub dim: Color,
    pub accent: Color,
    pub accent2: Color,
    pub good: Color,
    pub warn: Color,
    pub bad: Color,
    pub sel_bg: Color,
    pub sel_fg: Color,
    pub surface: Color,
    pub overlay: Color,
    pub cpu_low: Color,
    pub cpu_mid: Color,
    pub cpu_high: Color,
    pub mem_low: Color,
    pub mem_mid: Color,
    pub mem_high: Color,
}

fn hex(s: &str) -> Color {
    let s = s.trim_start_matches('#');
    if s.len() == 6 {
        let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(0);
        let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(0);
        let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(0);
        Color::Rgb(r, g, b)
    } else {
        Color::Rgb(0, 0, 0)
    }
}

fn to_hex(c: Color) -> String {
    if let Color::Rgb(r, g, b) = c {
        format!("#{:02x}{:02x}{:02x}", r, g, b)
    } else {
        "#000000".into()
    }
}

impl Theme {
    fn from_fields(name: &str, fields: &[(&str, &str)]) -> Self {
        fn val<'a>(fields: &'a [(&str, &str)], key: &str) -> &'a str {
            fields
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| *v)
                .unwrap_or("#000000")
        }
        Self {
            name: name.into(),
            bg: hex(val(fields, "bg")),
            panel: hex(val(fields, "panel")),
            panel2: hex(val(fields, "panel2")),
            border: hex(val(fields, "border")),
            border2: hex(val(fields, "border2")),
            txt: hex(val(fields, "txt")),
            dim: hex(val(fields, "dim")),
            accent: hex(val(fields, "accent")),
            accent2: hex(val(fields, "accent2")),
            good: hex(val(fields, "good")),
            warn: hex(val(fields, "warn")),
            bad: hex(val(fields, "bad")),
            sel_bg: hex(val(fields, "sel_bg")),
            sel_fg: hex(val(fields, "sel_fg")),
            surface: hex(val(fields, "surface")),
            overlay: hex(val(fields, "overlay")),
            cpu_low: hex(val(fields, "cpu_low")),
            cpu_mid: hex(val(fields, "cpu_mid")),
            cpu_high: hex(val(fields, "cpu_high")),
            mem_low: hex(val(fields, "mem_low")),
            mem_mid: hex(val(fields, "mem_mid")),
            mem_high: hex(val(fields, "mem_high")),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_fields(
            "default",
            &[
                ("bg", "#0d1116"),
                ("panel", "#161b22"),
                ("panel2", "#1a2029"),
                ("border", "#2d3542"),
                ("border2", "#3b4555"),
                ("txt", "#c9d1d9"),
                ("dim", "#747d88"),
                ("accent", "#7fc8f8"),
                ("accent2", "#5ba0d0"),
                ("good", "#59d09e"),
                ("warn", "#edc76e"),
                ("bad", "#f46d6d"),
                ("sel_bg", "#2a3c52"),
                ("sel_fg", "#eef0f4"),
                ("surface", "#1e2630"),
                ("overlay", "#0d1116"),
                ("cpu_low", "#59d09e"),
                ("cpu_mid", "#edc76e"),
                ("cpu_high", "#f46d6d"),
                ("mem_low", "#59d09e"),
                ("mem_mid", "#edc76e"),
                ("mem_high", "#f46d6d"),
            ],
        )
    }
}

impl Theme {
    pub fn to_fields(&self) -> Vec<(&str, String)> {
        vec![
            ("bg", to_hex(self.bg)),
            ("panel", to_hex(self.panel)),
            ("panel2", to_hex(self.panel2)),
            ("border", to_hex(self.border)),
            ("border2", to_hex(self.border2)),
            ("txt", to_hex(self.txt)),
            ("dim", to_hex(self.dim)),
            ("accent", to_hex(self.accent)),
            ("accent2", to_hex(self.accent2)),
            ("good", to_hex(self.good)),
            ("warn", to_hex(self.warn)),
            ("bad", to_hex(self.bad)),
            ("sel_bg", to_hex(self.sel_bg)),
            ("sel_fg", to_hex(self.sel_fg)),
            ("surface", to_hex(self.surface)),
            ("overlay", to_hex(self.overlay)),
            ("cpu_low", to_hex(self.cpu_low)),
            ("cpu_mid", to_hex(self.cpu_mid)),
            ("cpu_high", to_hex(self.cpu_high)),
            ("mem_low", to_hex(self.mem_low)),
            ("mem_mid", to_hex(self.mem_mid)),
            ("mem_high", to_hex(self.mem_high)),
        ]
    }
}

pub fn dracula() -> Theme {
    Theme::from_fields(
        "dracula",
        &[
            ("bg", "#1e1e2e"),
            ("panel", "#252536"),
            ("panel2", "#2a2a3d"),
            ("border", "#363650"),
            ("border2", "#45456a"),
            ("txt", "#cdd6f4"),
            ("dim", "#6c7086"),
            ("accent", "#89b4fa"),
            ("accent2", "#74c7ec"),
            ("good", "#a6e3a1"),
            ("warn", "#f9e2af"),
            ("bad", "#f38ba8"),
            ("sel_bg", "#45475a"),
            ("sel_fg", "#cdd6f4"),
            ("surface", "#313244"),
            ("overlay", "#1e1e2e"),
            ("cpu_low", "#a6e3a1"),
            ("cpu_mid", "#f9e2af"),
            ("cpu_high", "#f38ba8"),
            ("mem_low", "#a6e3a1"),
            ("mem_mid", "#f9e2af"),
            ("mem_high", "#f38ba8"),
        ],
    )
}

pub fn gruvbox() -> Theme {
    Theme::from_fields(
        "gruvbox",
        &[
            ("bg", "#282828"),
            ("panel", "#32302f"),
            ("panel2", "#3c3836"),
            ("border", "#504945"),
            ("border2", "#665c54"),
            ("txt", "#ebdbb2"),
            ("dim", "#928374"),
            ("accent", "#83a598"),
            ("accent2", "#8ec07c"),
            ("good", "#b8bb26"),
            ("warn", "#fabd2f"),
            ("bad", "#fb4934"),
            ("sel_bg", "#504945"),
            ("sel_fg", "#ebdbb2"),
            ("surface", "#3c3836"),
            ("overlay", "#282828"),
            ("cpu_low", "#b8bb26"),
            ("cpu_mid", "#fabd2f"),
            ("cpu_high", "#fb4934"),
            ("mem_low", "#b8bb26"),
            ("mem_mid", "#fabd2f"),
            ("mem_high", "#fb4934"),
        ],
    )
}

pub fn nord() -> Theme {
    Theme::from_fields(
        "nord",
        &[
            ("bg", "#2e3440"),
            ("panel", "#3b4252"),
            ("panel2", "#434c5e"),
            ("border", "#4c566a"),
            ("border2", "#616e88"),
            ("txt", "#d8dee9"),
            ("dim", "#81a1c1"),
            ("accent", "#88c0d0"),
            ("accent2", "#5e81ac"),
            ("good", "#a3be8c"),
            ("warn", "#ebcb8b"),
            ("bad", "#bf616a"),
            ("sel_bg", "#434c5e"),
            ("sel_fg", "#eceff4"),
            ("surface", "#3b4252"),
            ("overlay", "#2e3440"),
            ("cpu_low", "#a3be8c"),
            ("cpu_mid", "#ebcb8b"),
            ("cpu_high", "#bf616a"),
            ("mem_low", "#a3be8c"),
            ("mem_mid", "#ebcb8b"),
            ("mem_high", "#bf616a"),
        ],
    )
}

pub fn monokai() -> Theme {
    Theme::from_fields(
        "monokai",
        &[
            ("bg", "#272822"),
            ("panel", "#2e2f2a"),
            ("panel2", "#383830"),
            ("border", "#49483e"),
            ("border2", "#5b5a50"),
            ("txt", "#f8f8f2"),
            ("dim", "#75715e"),
            ("accent", "#66d9ef"),
            ("accent2", "#a6e22e"),
            ("good", "#a6e22e"),
            ("warn", "#e6db74"),
            ("bad", "#f92672"),
            ("sel_bg", "#49483e"),
            ("sel_fg", "#f8f8f2"),
            ("surface", "#383830"),
            ("overlay", "#272822"),
            ("cpu_low", "#a6e22e"),
            ("cpu_mid", "#e6db74"),
            ("cpu_high", "#f92672"),
            ("mem_low", "#a6e22e"),
            ("mem_mid", "#e6db74"),
            ("mem_high", "#f92672"),
        ],
    )
}

pub fn solarized() -> Theme {
    Theme::from_fields(
        "solarized",
        &[
            ("bg", "#002b36"),
            ("panel", "#073642"),
            ("panel2", "#093f4a"),
            ("border", "#586e75"),
            ("border2", "#657b83"),
            ("txt", "#839496"),
            ("dim", "#586e75"),
            ("accent", "#268bd2"),
            ("accent2", "#2aa198"),
            ("good", "#859900"),
            ("warn", "#b58900"),
            ("bad", "#dc322f"),
            ("sel_bg", "#073642"),
            ("sel_fg", "#93a1a1"),
            ("surface", "#073642"),
            ("overlay", "#002b36"),
            ("cpu_low", "#859900"),
            ("cpu_mid", "#b58900"),
            ("cpu_high", "#dc322f"),
            ("mem_low", "#859900"),
            ("mem_mid", "#b58900"),
            ("mem_high", "#dc322f"),
        ],
    )
}

pub fn catppuccin() -> Theme {
    Theme::from_fields(
        "catppuccin",
        &[
            ("bg", "#1e1e2e"),
            ("panel", "#262637"),
            ("panel2", "#2e2e44"),
            ("border", "#45456a"),
            ("border2", "#585b70"),
            ("txt", "#cdd6f4"),
            ("dim", "#6c7086"),
            ("accent", "#89b4fa"),
            ("accent2", "#b4befe"),
            ("good", "#a6e3a1"),
            ("warn", "#f9e2af"),
            ("bad", "#f38ba8"),
            ("sel_bg", "#45475a"),
            ("sel_fg", "#cdd6f4"),
            ("surface", "#313244"),
            ("overlay", "#1e1e2e"),
            ("cpu_low", "#a6e3a1"),
            ("cpu_mid", "#f9e2af"),
            ("cpu_high", "#f38ba8"),
            ("mem_low", "#a6e3a1"),
            ("mem_mid", "#f9e2af"),
            ("mem_high", "#f38ba8"),
        ],
    )
}

pub fn tokyo_night() -> Theme {
    Theme::from_fields(
        "tokyo-night",
        &[
            ("bg", "#1a1b26"),
            ("panel", "#21222d"),
            ("panel2", "#282938"),
            ("border", "#363b54"),
            ("border2", "#444b6a"),
            ("txt", "#a9b1d6"),
            ("dim", "#565f89"),
            ("accent", "#7aa2f7"),
            ("accent2", "#bb9af7"),
            ("good", "#9ece6a"),
            ("warn", "#e0af68"),
            ("bad", "#f7768e"),
            ("sel_bg", "#363b54"),
            ("sel_fg", "#c0caf5"),
            ("surface", "#282938"),
            ("overlay", "#1a1b26"),
            ("cpu_low", "#9ece6a"),
            ("cpu_mid", "#e0af68"),
            ("cpu_high", "#f7768e"),
            ("mem_low", "#9ece6a"),
            ("mem_mid", "#e0af68"),
            ("mem_high", "#f7768e"),
        ],
    )
}

pub fn one_dark() -> Theme {
    Theme::from_fields(
        "one-dark",
        &[
            ("bg", "#282c34"),
            ("panel", "#2f333d"),
            ("panel2", "#353b45"),
            ("border", "#4b5263"),
            ("border2", "#5c6370"),
            ("txt", "#abb2bf"),
            ("dim", "#5c6370"),
            ("accent", "#61afef"),
            ("accent2", "#56b6c2"),
            ("good", "#98c379"),
            ("warn", "#e5c07b"),
            ("bad", "#e06c75"),
            ("sel_bg", "#3e4452"),
            ("sel_fg", "#abb2bf"),
            ("surface", "#353b45"),
            ("overlay", "#282c34"),
            ("cpu_low", "#98c379"),
            ("cpu_mid", "#e5c07b"),
            ("cpu_high", "#e06c75"),
            ("mem_low", "#98c379"),
            ("mem_mid", "#e5c07b"),
            ("mem_high", "#e06c75"),
        ],
    )
}

pub fn everforest() -> Theme {
    Theme::from_fields(
        "everforest",
        &[
            ("bg", "#2b3339"),
            ("panel", "#323d43"),
            ("panel2", "#3a474e"),
            ("border", "#4b565c"),
            ("border2", "#5d6a72"),
            ("txt", "#d3c6aa"),
            ("dim", "#859289"),
            ("accent", "#7fbbb3"),
            ("accent2", "#a7c080"),
            ("good", "#a7c080"),
            ("warn", "#dbbc7f"),
            ("bad", "#e67e80"),
            ("sel_bg", "#475259"),
            ("sel_fg", "#d3c6aa"),
            ("surface", "#3a474e"),
            ("overlay", "#2b3339"),
            ("cpu_low", "#a7c080"),
            ("cpu_mid", "#dbbc7f"),
            ("cpu_high", "#e67e80"),
            ("mem_low", "#a7c080"),
            ("mem_mid", "#dbbc7f"),
            ("mem_high", "#e67e80"),
        ],
    )
}

pub fn builtin_themes() -> Vec<Theme> {
    vec![
        Theme::default(),
        dracula(),
        gruvbox(),
        nord(),
        monokai(),
        solarized(),
        catppuccin(),
        tokyo_night(),
        one_dark(),
        everforest(),
    ]
}

pub fn list_theme_names() -> Vec<String> {
    let mut names: Vec<String> = builtin_themes().into_iter().map(|t| t.name).collect();
    if let Ok(dir) = betterssh_core::themes_dir() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|ext| ext == "toml") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        let n = stem.to_string();
                        if !names.contains(&n) {
                            names.push(n);
                        }
                    }
                }
            }
        }
    }
    names
}

pub fn load_theme(name: &str) -> Theme {
    for t in builtin_themes() {
        if t.name == name {
            return t;
        }
    }
    if let Ok(dir) = betterssh_core::themes_dir() {
        let p = dir.join(format!("{}.toml", name));
        if let Ok(raw) = std::fs::read_to_string(&p) {
            if let Ok(val) = raw.parse::<toml::Value>() {
                if let Some(tab) = val.as_table() {
                    let fields: Vec<(&str, &str)> = THEME_KEYS
                        .iter()
                        .filter_map(|k| tab.get(*k).and_then(|v| v.as_str()).map(|v| (*k, v)))
                        .collect();
                    if !fields.is_empty() {
                        return Theme::from_fields(name, &fields);
                    }
                }
            }
        }
    }
    Theme::default()
}

const THEME_KEYS: &[&str] = &[
    "bg", "panel", "panel2", "border", "border2", "txt", "dim", "accent", "accent2", "good",
    "warn", "bad", "sel_bg", "sel_fg", "surface", "overlay", "cpu_low", "cpu_mid", "cpu_high",
    "mem_low", "mem_mid", "mem_high",
];
