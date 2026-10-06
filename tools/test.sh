#!/bin/sh
# Fast, firmware-independent regression suite for the core, both hosts, and panel.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)

cargo test --manifest-path "$root/xwp1/Cargo.toml" --lib
cargo test --manifest-path "$root/xwp1/Cargo.toml" --test panel_integration
cargo test --manifest-path "$root/xwp1-plugin/Cargo.toml" --lib
cargo test --manifest-path "$root/xwp1-app/Cargo.toml"

for file in "$root"/xwp1/panel/*.js; do
    node --check "$file"
done
node --test "$root"/tests/*.test.cjs
