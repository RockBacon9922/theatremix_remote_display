#!/usr/bin/env bash
set -euo pipefail

BIN_NAME="theatremix-remote-display"
BIN_PATH_A="target/aarch64-apple-darwin/release/${BIN_NAME}"
BIN_PATH_X="target/x86_64-apple-darwin/release/${BIN_NAME}"

echo "Installing Rust targets (if missing)..."
rustup target add aarch64-apple-darwin x86_64-apple-darwin

echo "Building release binaries..."
cargo build --release --target aarch64-apple-darwin --bin "${BIN_NAME}"
cargo build --release --target x86_64-apple-darwin --bin "${BIN_NAME}"

echo "Checking build outputs..."
if [[ ! -f "${BIN_PATH_A}" || ! -f "${BIN_PATH_X}" ]]; then
  echo "Expected binaries not found."
  echo "Listing matching binaries under target/:"
  find target -maxdepth 5 -type f -name "${BIN_NAME}" -print
  echo "Listing target/ tree (top level):"
  ls -la target
  exit 1
fi

echo "Creating universal binary..."
mkdir -p target/release
lipo -create \
  "${BIN_PATH_A}" \
  "${BIN_PATH_X}" \
  -output "target/release/${BIN_NAME}"

echo "Verifying universal binary..."
file "target/release/${BIN_NAME}"
lipo -info "target/release/${BIN_NAME}"

echo "Packaging dmg..."
if ! command -v cargo-packager >/dev/null 2>&1; then
  echo "Installing cargo-packager..."
  cargo install cargo-packager --locked
fi

cargo packager --release --formats dmg

echo "macOS build test and dmg packaging complete."
