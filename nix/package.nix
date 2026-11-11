{ lib, rustPlatform }:
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
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

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
