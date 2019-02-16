# nix-user-chroot
[![Build Status](https://travis-ci.com/nix-community/nix-user-chroot.svg?branch=master)](https://travis-ci.com/nix-community/nix-user-chroot)

Rust rewrite of
[lethalman's version](https://github.com/lethalman/nix-user-chroot)
to clarify the license situation.
This forks also makes it possible to use the nix sandbox!

Run and install nix as user without root permissions. Nix-user-chroot requires
user namespaces to perform its task (available since linux 3.8). Note that this
is not available for unprivileged users in some Linux distributions such as
Red Hat Linux, CentOS and Archlinux when using the stock kernel. It should be
available in Ubuntu and Debian.

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

On debian-based system this feature might be disabled by default.
However they provide a [sysctl switch](https://superuser.com/a/1122977)
to enable it at runtime.

On RedHat / CentOS 7.4 user namespaces are disabled by default, but can be
enabled by:

1. Adding `namespace.unpriv_enable=1` to the kernel boot parameters via `grubby`
2. `echo "user.max_user_namespaces=15076" >> /etc/sysctl.conf` to increase the
number of allowed namespaces above the default 0.

For more details, see the
[RedHat Documentation](https://access.redhat.com/documentation/en-us/red_hat_enterprise_linux_atomic_host/7/html-single/getting_started_with_containers/index#user_namespaces_options)

## Download static binaries

Checkout the [latest release](https://github.com/nix-community/nix-user-chroot/releases/latest)
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

This will download and extract latest nix binary tarball from the chroot:

```console
$ mkdir -m 0755 ~/.nix
$ nix-user-chroot ~/.nix bash -c "curl https://nixos.org/nix/install | bash"
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

## Whishlist

These are features the author would like to see, let me know, if you want to work
on this:

### Add an `--install` flag:

Instead of

```console
$ mkdir -m 0755 ~/.nix
$ nix-user-chroot ~/.nix bash -c "curl https://nixos.org/nix/install | bash"
```

it should just be:

```console
$ nix-user-chroot --install
```

This assumes we just install to `$XDG_DATA_HOME` or `$HOME/.data/nix` by default.

### Add a setuid version

Since not all linux distributions allow user namespaces by default, we will need
packages for those that install setuid binaries to achieve the same.
