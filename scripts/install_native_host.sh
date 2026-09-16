#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cargo build --release --manifest-path "$repo_dir/Cargo.toml"
native_binary="$repo_dir/scripts/native-host.sh"

case "$(uname -s)" in
  Darwin)
    manifest_dir="${HOME}/Library/Application Support/Mozilla/NativeMessagingHosts"
    ;;
  Linux)
    manifest_dir="${HOME}/.mozilla/native-messaging-hosts"
    ;;
  *)
    echo "error: automatic native-host installation is supported on macOS and Linux" >&2
    exit 1
    ;;
esac

mkdir -p "$manifest_dir"
manifest_path="$manifest_dir/com.downer.native.json"
printf '{\n  "name": "com.downer.native",\n  "description": "Downer FFmpeg native messaging host",\n  "path": "%s",\n  "type": "stdio",\n  "allowed_extensions": ["downer@example.com"]\n}\n' "$native_binary" > "$manifest_path"
chmod 755 "$native_binary" "$repo_dir/target/release/downer"
echo "Installed native host manifest: $manifest_path"
