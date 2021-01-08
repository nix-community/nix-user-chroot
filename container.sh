ENGINE=docker

if ! which "$ENGINE" &>/dev/null ; then
    ENGINE=podman
fi

if ! which "$ENGINE" &>/dev/null ; then
    echo "No docker or podman in PATH" >&2
    exit 1
fi

read -d '' script <<EOF
cd /app/nix-user-chroot
mkdir .nix
cargo build

run() {
    cargo run .nix "\$@"
}

export -f run
exec bash
EOF

# "$ENGINE" run -v .:/app -it rust bash -c "$script"
"$ENGINE" run -v ..:/app -it rust bash -c "$script"
