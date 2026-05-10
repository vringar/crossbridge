{ lib
, rustPlatform
, pkg-config
, sqlite
}:

rustPlatform.buildRustPackage {
  pname = "crossbridge";
  version = "0.1.0";

  src = lib.cleanSource ./.;

  cargoLock = {
    lockFile = ./Cargo.lock;
    allowBuiltinFetchGit = true;
  };

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ sqlite ];

  meta = with lib; {
    description = "Cross-project coordination bridge for crosslink repositories";
    license = licenses.mit;
    mainProgram = "crossbridge-supervisor";
  };
}
