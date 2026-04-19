# Rustinator

A GPU-accelerated terminal emulator inspired by Terminator, built with Rust. Supports tabbed and split-pane layouts, transparency, subpixel font rendering, and configurable keybindings.

## Build

```bash
sudo apt install libfontconfig1-dev libfreetype-dev libxkbcommon-dev libegl1-mesa-dev libxcb1-dev
cargo build --release
```

## Run

```bash
cargo run --release
# or directly:
./target/release/rustinator
```

## Install

Copy the binary and optionally create a desktop entry:

```bash
sudo cp target/release/rustinator /usr/local/bin/
```

```ini
# ~/.local/share/applications/rustinator.desktop
[Desktop Entry]
Name=Rustinator
Exec=/usr/local/bin/rustinator
Type=Application
Terminal=false
Categories=System;TerminalEmulator;
```

## Config

`~/.config/rustinator/config.toml` is created on first use. See `keybindings.md` for shortcuts.
