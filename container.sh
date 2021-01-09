#!/usr/bin/env bash

cd "$(dirname "${BASH_SOURCE[0]}")"

if [[ -z $ENGINE ]]; then
  if command -v podman &>/dev/null; then
    ENGINE=podman
  elif command -v docker &>/dev/null; then
    ENGINE=docker
  else
    echo "No podman or docker in PATH" >&2
    exit 1
  fi
fi

read -d '' script <<EOF
cd /app
mkdir .nix
cargo build

run() {
  cargo run .nix "\$@"
}

export -f run
exec bash
EOF

"$ENGINE" run -v $PWD:/app --privileged -it rust bash -c "$script"
