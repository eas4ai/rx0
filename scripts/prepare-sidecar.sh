#!/bin/sh
# Stage the rx0 server as a Tauri sidecar: build it, then copy it into
# src-tauri/binaries with the target-triple suffix Tauri requires
# (plus .exe on Windows). Usage: prepare-sidecar.sh [debug|release].
set -eu
profile="${1:-debug}"
case "$profile" in
  release) cargo_flag="--release" ;;
  *) profile="debug"; cargo_flag="" ;;
esac
triple="$(rustc -vV | sed -n 's/^host: //p')"
ext=""
case "$triple" in *-windows-*) ext=".exe" ;; esac
# Plain `cargo build` has no triple subdir; --target builds do.
cargo build $cargo_flag
root="${CARGO_TARGET_DIR:-target}"
if [ -x "$root/$triple/$profile/rx0$ext" ]; then
  src="$root/$triple/$profile/rx0$ext"
else
  src="$root/$profile/rx0$ext"
fi
mkdir -p src-tauri/binaries
cp "$src" "src-tauri/binaries/rx0-sidecar-$triple$ext"
echo "sidecar: src-tauri/binaries/rx0-sidecar-$triple$ext"
