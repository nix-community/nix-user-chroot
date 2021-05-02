with import <nixpkgs> {};
stdenv.mkDerivation {
  name = "env";
  buildInputs = [
    bashInteractive
    rustup
  ];
  RUST_SRC_PATH = rustPlatform.rustcSrc;
}
