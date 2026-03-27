# nix-user-chroot
[![CI](https://github.com/nix-community/nix-user-chroot/actions/workflows/ci.yml/badge.svg)](https://github.com/nix-community/nix-user-chroot/actions/workflows/ci.yml)

Rust rewrite of
[lethalman's version](https://github.com/lethalman/nix-user-chroot)
to clarify the license situation.
This forks also makes it possible to use the nix sandbox!

Run and install nix as user without root permissions. Nix-user-chroot requires
unprivileged user namespaces (available since Linux 3.8). Most distributions
support this but some restrict it by default — notably Ubuntu 23.10+ gates it
behind AppArmor, and RHEL/CentOS 7 ship with it disabled. See below for how to
check and enable it on your system.

## Check if your kernel supports user namespaces for unprivileged users

```console
$ unshare --user --pid echo YES
YES
```

The output should be <code>YES</code>.
If the command is absent, an alternative is to check the kernel compile options:

```console
$ zgrep CONFIG_USER_NS /proc/config.gz
CONFIG_USER_NS=y
```

On some systems, like Debian or Ubuntu, the kernel configuration is in a different place, so instead use:

```console
$ grep CONFIG_USER_NS /boot/config-$(uname -r)
CONFIG_USER_NS=y
```

You can also try reading `/proc/sys/kernel/unprivileged_userns_clone`. This flag should be present, and set to `1`:

```console
$ cat /proc/sys/kernel/unprivileged_userns_clone
1
```

### Enabling user namespaces

**Ubuntu 23.10+** restricts unprivileged user namespaces via AppArmor. Disable
the restriction with:

```console
$ sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
```

To make it persistent, drop that line (without `sudo sysctl -w`) into
`/etc/sysctl.d/99-userns.conf`. See
[Ubuntu's security docs](https://ubuntu.com/blog/ubuntu-23-10-restricted-unprivileged-user-namespaces)
for the rationale and per-binary alternatives.

**Older Debian / Arch** may have `kernel.unprivileged_userns_clone=0`; flip it
to `1` via sysctl. Modern releases enable it by default.

**RHEL / CentOS 7** ships with it disabled at the kernel level:

1. Add `namespace.unpriv_enable=1` to the kernel boot parameters via `grubby`
2. `echo "user.max_user_namespaces=15076" >> /etc/sysctl.conf`

RHEL 8+ enables it by default.

Note that unprivileged user namespaces have
[a history of kernel CVEs](https://security.stackexchange.com/questions/209529);
the distributions that restrict them do so deliberately.

## Download static binaries

Checkout the [latest release](https://github.com/nix-community/nix-user-chroot/releases/latest)
and download the binary matching your architecture.

## Install with cargo

``` console
$ cargo install nix-user-chroot
```

## Build from source

```console
$ git clone https://github.com/nix-community/nix-user-chroot
$ cd nix-user-chroot
$ cargo build --release
```

If you use rustup, you can also build a statically linked version:

```console
$ rustup target add x86_64-unknown-linux-musl
$ cargo build --release --target=x86_64-unknown-linux-musl
```

## Installation

This will download and extract latest nix binary tarball from the chroot:

```console
$ mkdir -m 0755 ~/.nix
$ nix-user-chroot ~/.nix bash -c "curl -L https://nixos.org/nix/install | bash"
```

The installation described here will not work on NixOS this way, because you
start with an empty nix store and miss therefore tools like bash and coreutils.
You won't need `nix-user-chroot` on NixOS anyway since you can get similar
functionality using `nix run --store ~/.nix nixpkgs.bash nixpkgs.coreutils`:

## Usage

After installation you can always get into the nix user chroot using:

```console
$ nix-user-chroot ~/.nix bash -l
```

You are in a user chroot where `/` is owned by your user, hence also `/nix` is
owned by your user. Everything else is bind mounted from the real root.

The nix config is not in `/etc/nix` but in `/nix/etc/nix`, so that you can
modify it. This is done with the `NIX_CONF_DIR`, which you can override at any
time.

Libraries and applications from Nixpkgs with OpenGL or CUDA support need to
load libraries from /run/opengl-driver/lib. For convenience, nix-user-chroot
will bind mount /nix/var/nix/opengl-driver/lib (if it exists) to this location.
You will still need to link the system libraries here, as their original
locations are distro-dependent. For example, for CUDA support on Ubuntu 20.04:

```console
$ mkdir -p /nix/var/nix/opengl-driver/lib
$ ln -s /usr/lib/x86_64-linux-gnu/libcuda.so.1 /nix/var/nix/opengl-driver/lib
```

If this directory didn't exist when you first entered the nix user chroot, you
will need to reenter for /run/opengl-driver/lib to be mounted.

## Configuration

nix-user-chroot reads an optional TOML config file from
`<nixpath>/etc/nix-user-chroot/path-config.toml` (e.g.
`~/.nix/etc/nix-user-chroot/path-config.toml`). All sections are optional.

```toml
[excludes]
# Absolute paths that should NOT be mirrored into the chroot.
# Useful for things like nscd sockets that break nix's own resolver.
paths = [
    "/var/run/nscd/socket",
]

[profile]
# Mount paths from your per-user nix profile
# (<nixpath>/var/nix/profiles/per-user/$USER/profile/...) into the chroot.
# Keys are profile-relative (leading / is stripped), values are absolute
# destinations in the chroot.
"bin/env" = "/usr/bin/env"

[absolute]
# Mount arbitrary host paths into the chroot.
# Both keys (sources) and values (destinations) must be absolute.
"/home/me/chroot-passwd" = "/etc/passwd"
```

## Wishlist

PRs welcome:

- **`--install` flag** — collapse `mkdir ~/.nix && nix-user-chroot ~/.nix bash -c "curl ... | bash"` into a single command that defaults to `$XDG_DATA_HOME/nix`.
- **home-manager integration** — a module that generates `path-config.toml` so users get working defaults (nscd exclusion, `/usr/bin/env`, etc.) without hand-writing TOML. See the [discussion in #75](https://github.com/nix-community/nix-user-chroot/pull/75).
- **Directory excludes** — the current exclude mechanism bind-mounts `/dev/null` as a placeholder, which only works for files. Excluding a directory needs a different approach (empty tmpfs or similar).
- **setuid fallback** — for systems where unprivileged user namespaces are restricted (Ubuntu 23.10+, locked-down servers) and the user can't change sysctls. A setuid helper could set up the namespace on their behalf.

## Similar projects

- [nix-portable](https://github.com/DavHau/nix-portable) — zero-config, also works without unprivileged user namespaces (via proot fallback). Better suited for distributing nix-based tools; nix-user-chroot is aimed at standing up a persistent nix environment on a machine where you lack root.
- [bwrap](https://github.com/containers/bubblewrap) — the general-purpose sandbox primitive. nix-user-chroot is essentially a nix-aware preset on top of the same kernel APIs.
