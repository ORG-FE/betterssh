# betterssh

TUI SSH connection manager in tmux/screen style.

Built on [ratatui](https://github.com/ratatui-org/ratatui) and [russh](https://github.com/warp-tech/russh).

## Features

- Multiple SSH sessions in tabs
- SSH config import (`~/.ssh/config`)
- Connection groups with collapsible sections
- Per-host on-connect commands
- Command palette (Ctrl+P)
- Search in terminal output (Ctrl+F)
- Session rename
- SFTP file browser
- Custom themes (10 built-in + custom `.toml`)
- Capture mode for terminal output
- Mouse forwarding
- Keybinding overrides
- Macro system
- Port forwarding (local, remote, dynamic/SOCKS5)
- Vault for password storage
- Snippet manager

## Installation

### From source

```bash
cargo install --git https://github.com/ORG-FE/betterssh
```

### From releases

Download the archive for your platform from the [releases page](https://github.com/ORG-FE/betterssh/releases), extract and run `betterssh`.

### Build locally

```bash
git clone https://github.com/ORG-FE/betterssh.git
cd betterssh
cargo build --release
./target/release/betterssh
```

## Quick start

```bash
# create default config
betterssh init

# edit config directly
betterssh edit

# print current config
betterssh print

# start TUI (default)
betterssh
```

Add hosts to `~/.config/betterssh/hosts.toml` or press `n` in the host list to add one interactively. Import existing SSH config with `i`.

## CLI

| Command   | Description |
|-----------|-------------|
| (none)    | Start TUI   |
| `edit`    | Open config in `$EDITOR` |
| `print`   | Print current config as TOML |
| `init`    | Write default config file |

## Configuration

Config file: `~/.config/betterssh/hosts.toml`

### Host entry

```toml
[[host]]
name = "server"
host = "192.168.1.100"
port = 22
user = "root"
group = "production"
tags = ["web", "backend"]
keepalive = 30
on_connect = ["htop"]

[[host.identity]]
key = { path = "~/.ssh/id_rsa", passphrase = "" }

[[host.forwarding]]
direction = "local"
listen_addr = "127.0.0.1"
listen_port = 8080
target_host = "localhost"
target_port = 80
```

### Settings

```toml
[settings]
theme = "dracula"
default_user = "root"
ping_check = false
auto_reconnect = true
scrollback = 10000
mouse = true
show_metrics = true

[settings.keybindings]
command_palette = "ctrl+p"
quit = "ctrl+q"
save_config = "ctrl+s"

[[settings.macros]]
name = "update"
commands = ["apt update", "apt upgrade -y"]
key = "ctrl+u"
```

## Keybindings

| Key          | Action                |
|--------------|-----------------------|
| Ctrl+P       | Command palette       |
| Ctrl+F       | Search in terminal    |
| Ctrl+S       | SFTP browser          |
| Ctrl+Q/W/T   | Close tab             |
| Ctrl+N       | New host              |
| Ctrl+B/Ctrl+\| Toggle capture mode   |
| Alt+M        | Toggle mouse forward  |
| Tab/Shift+Tab| Cycle sessions        |
| F2           | Rename session / Settings |
| F1/F12       | Snippets              |
| [ / ]        | Switch session        |
| i            | Import SSH config     |
| n            | New host (in list)    |
| g            | Toggle group mode     |
| Enter        | Connect to host       |
| PageUp/Down  | Scroll terminal       |
| /            | Filter host list      |

## Themes

Built-in themes: `default`, `dracula`, `gruvbox`, `nord`, `monokai`, `solarized`, `catppuccin`, `tokyo-night`, `one-dark`, `everforest`.

Custom themes: place `.toml` files in `~/.config/betterssh/themes/`.

## License

MIT
