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

# macOS 15+ silently denies local network access without this key.
plutil -replace NSLocalNetworkUsageDescription \
    -string "Rustinator runs shell commands that may reach devices on your local network." \
    "${APP}/Contents/Info.plist"

# Fixed identifier so TCC grants survive rebuilds (default ad-hoc id includes a hash).
codesign --force --deep --sign "Developer ID Application" \
    --options runtime --timestamp --identifier com.rustinator.app "${APP}"

echo "Installing to /Applications/..."
rm -rf /Applications/Rustinator.app
cp -r "${APP}" /Applications/
echo "Installed to /Applications/Rustinator.app"
