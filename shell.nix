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
  RUST_SRC_PATH = pkgs.rustPlatform.rustcSrc;
}
