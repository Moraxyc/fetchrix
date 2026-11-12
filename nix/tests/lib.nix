{
  lib,
  pkgs,
  runCommand,
  buildNpmPackage,
  importNpmLock,
  fetchrixLib,
}:
let
  overlaid = pkgs.extend (
    fetchrixLib.overlay {
      dir = ./_sources;
      attr = "mySources";
    }
  );
  inherit (overlaid.mySources) foo baz web;
  # The fixture source stands in for the unfetchable `web.src`.
  webApp = buildNpmPackage {
    inherit (web) pname version;
    src = ./npm-src;
    sourceRoot = "npm-src/frontend";
    npmDeps = importNpmLock (web.npmDeps // { npmRoot = ./npm-src/frontend; });
    npmConfigHook = importNpmLock.npmConfigHook;
    dontNpmBuild = true;
    installPhase = ''
      node -e 'process.exit(require("a") === "a" ? 0 : 1)'
      touch $out
    '';
  };
in
assert foo.pname == "foo-bin";
assert web.pname == "web";
assert foo.version == "1.2.3";
assert foo.rawVersion == "v1.2.3";
assert !(web ? rawVersion);
assert foo.src.outputHash == "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
assert foo.npmDepsHash == "sha256-CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=";
assert foo.cargoLock.lockFile == ./_sources/foo/src/Cargo.lock;
assert
  foo.cargoLock.outputHashes == {
    "bar-0.1.0" = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";
  };
assert web.npmDeps.npmRoot == "${web.src}/frontend";
assert web.npmDeps.package.name == "web";
assert web.npmDeps.packageLock.lockfileVersion == 3;
assert lib.hasInfix "baz.git" baz.src.url;
assert !baz.src.fetchSubmodules;
assert baz.date == "2024-01-15";
runCommand "fetchrix-lib-test" { } "test -e ${webApp} && touch $out"
