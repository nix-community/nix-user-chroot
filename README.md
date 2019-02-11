# nix-user-chroot
[![Build Status](https://travis-ci.com/nix-community/nix-user-chroot.svg?branch=master)](https://travis-ci.com/nix-community/nix-user-chroot)

Rust rewrite of 
[lethalman's version](https://github.com/lethalman/nix-user-chroot)
to clarify the license situation.
This forks also makes it possible to use the nix sandbox!

Run and install nix as user without root permissions. Nix-user-chroot requires
username spaces to perform its task (available since linux 3.8). Note that this
is not available for unprivileged users in some Linux distributions such as
Red Hat Linux, CentOS and Archlinux when using the stock kernel. It should be available
in Ubuntu and Debian.

## Download static binaries

Checkout the [releases tab](https://github.com/nix-community/nix-user-chroot/releases)
and download the binary matching your architecture.

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

```console
$ mkdir -m 0755 ~/.nix
$ nix-user-chroot ~/.nix bash
# download and extract latest nix binary tarball from the chroot
$ curl https://nixos.org/nix/install | bash
```

The installation described here will not work on NixOS this way, because you
start with an empty nix store and miss therefore tools like bash and coreutils.
You won't need `nix-user-chroot` on NixOS anyway since you can get similar
functionality using `nix run --store ~/.nix nixpkgs.bash nixpkgs.coreutils`:

## Usage

After installation you can always get into the nix user chroot using:

```console
$ nix-user-chroot ~/.nix bash
```

You are in a user chroot where `/` is owned by your user, hence also `/nix` is
owned by your user. Everything else is bind mounted from the real root.

The nix config is not in `/etc/nix` but in `/nix/etc/nix`, so that you can
modify it. This is done with the `NIX_CONF_DIR`, which you can override at any
time.
