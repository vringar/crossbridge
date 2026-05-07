{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    cargo
    rustc
    clippy
    rustfmt
    rust-analyzer
    pkg-config
    jujutsu
  ];

  buildInputs = with pkgs; [
    sqlite
  ];

  RUST_LOG = "crossbridge=debug";
}
