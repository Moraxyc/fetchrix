{
  lib,
  pkgs,
  runCommand,
  fetchrixLib,
}:
let
  overlaid = pkgs.extend (
    fetchrixLib.overlay {
      dir = ./_sources;
      attr = "mySources";
    }
  );
  inherit (overlaid.mySources) foo baz;
in
assert foo.version == "1.2.3";
assert foo.src.outputHash == "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
assert foo.npmDepsHash == "sha256-CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=";
assert foo.cargoLock.lockFile == ./_sources/foo/Cargo.lock;
assert
  foo.cargoLock.outputHashes == {
    "bar-0.1.0" = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";
  };
assert lib.hasInfix "baz.git" baz.src.url;
assert !baz.src.fetchSubmodules;
runCommand "fetchrix-lib-test" { } "touch $out"
