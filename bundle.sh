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

echo "Installing to /Applications/..."
rm -rf /Applications/Rustinator.app
cp -r "${APP}" /Applications/
echo "Installed to /Applications/Rustinator.app"
