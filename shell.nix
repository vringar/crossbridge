{ sources ? import ./npins
, pkgs ? import sources.nixpkgs {}
}:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    cargo
    rustc
    pkg-config
  ];

  buildInputs = with pkgs; [
    sqlite
  ];

  packages = with pkgs; [
    clippy
    rustfmt
    rust-analyzer
    jujutsu
  ];

  RUST_LOG = "crossbridge=debug";
}
