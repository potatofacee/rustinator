#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo-bundle &>/dev/null; then
    echo "Installing cargo-bundle..."
    cargo install cargo-bundle
fi

echo "Bundling release..."
cargo bundle --release

APP="target/release/bundle/osx/Rustinator.app"
echo "App bundle created at ${APP}"
echo "To install: cp -r \"${APP}\" /Applications/"
