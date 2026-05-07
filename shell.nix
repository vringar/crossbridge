{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    cargo
    rustc
    rustfmt
    clippy
    pkg-config
    jujutsu
  ];

  buildInputs = with pkgs; [
    sqlite
  ];

  RUST_LOG = "crossbridge=debug";
}
