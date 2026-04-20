# Rustinator

A GPU-accelerated terminal emulator inspired by Terminator, built with Rust. Supports tabbed and split-pane layouts, transparency, subpixel font rendering, and configurable keybindings.

## Prerequisites

Requires Rust 1.85+ (edition 2024). Install via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

If you already have Rust installed, make sure it's up to date:

```bash
rustup update
```

### Linux (Debian/Ubuntu)

```bash
sudo apt install libfontconfig1-dev libfreetype-dev libxkbcommon-dev libegl1-mesa-dev libxcb1-dev
```

### Linux (Fedora)

```bash
sudo dnf install fontconfig-devel freetype-devel libxkbcommon-devel mesa-libEGL-devel libxcb-devel
```

### Linux (Arch)

```bash
sudo pacman -S fontconfig freetype2 libxkbcommon mesa libxcb
```

### macOS

No additional system dependencies. Xcode Command Line Tools are required:

```bash
xcode-select --install
```

## Build

```bash
cargo build --release
```

## Run

```bash
cargo run --release
# or directly:
./target/release/rustinator
```

## Install

### Linux

```bash
sudo cp target/release/rustinator /usr/local/bin/
```

Optional desktop entry:

```ini
# ~/.local/share/applications/rustinator.desktop
[Desktop Entry]
Name=Rustinator
Exec=/usr/local/bin/rustinator
Type=Application
Terminal=false
Categories=System;TerminalEmulator;
```

### macOS

```bash
cp target/release/rustinator /usr/local/bin/
```

## Config

`~/.config/rustinator/config.toml` is created on first use. See `keybindings.md` for shortcuts.
