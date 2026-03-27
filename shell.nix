{
  pkgs ? import <nixpkgs> { },
}:
pkgs.stdenv.mkDerivation {
  name = "env";
  buildInputs = with pkgs; [
    bashInteractive
    cargo
    rustc
  ];
  # Used by tests/path_config.rs to get a shell that works inside a
  # chroot with a fake (empty) nixdir, where /bin/sh -> /nix/store/...
  # would be dangling.
  NIX_USER_CHROOT_TEST_BUSYBOX = "${pkgs.pkgsStatic.busybox}/bin/busybox";
}
