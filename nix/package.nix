{
  lib,
  rustPlatform,
  aws-lc,
  gitMinimal,
  pkg-config,
}:
let
  manifest = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = manifest.package.name;
  inherit (manifest.package) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ./tests/_sources/lock.json
    ];
  };

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ aws-lc ];
  nativeCheckInputs = [ gitMinimal ];

  cargoLock = {
    lockFile = ../Cargo.lock;
    outputHashes."nix-compat-0.1.0" = "sha256-ZMU90InalYjEe9Uvv0NCNCHNIxKmt4PRcWZ42w272Xk=";
  };

  meta = {
    inherit (manifest.package) description;
    homepage = manifest.package.repository;
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = manifest.package.name;
  };
}
