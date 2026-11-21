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
      config = ./fetchrix.toml;
      attr = "mySources";
    }
  );
  inherit (overlaid.mySources)
    foo
    baz
    new
    web
    ;
  gradleDepsDrv =
    data:
    (overlaid.gradle.fetchDeps {
      pkg.pname = "fetchrix-test";
      inherit data;
    }).drvPath;
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
assert foo.gradleDeps == ./_sources/foo/gradleDeps.json;
assert builtins.isPath baz.gradleDeps;
assert builtins.isPath new.gradleDeps;
assert (lib.importJSON baz.gradleDeps)."!version" == 1;
assert (lib.importJSON new.gradleDeps)."!version" == 1;
assert gradleDepsDrv baz.gradleDeps == gradleDepsDrv new.gradleDeps;
assert lib.isDerivation new.src;
assert new.src.name == "source";
assert foo.pname == "foo-bin";
assert web.pname == "web";
assert foo.version == "1.2.3";
assert foo.rawVersion == "v1.2.3";
assert web.rawVersion == "v0.1.0";
assert foo.src.outputHash == "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
assert foo.npmDepsHash == "sha256-CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=";
assert foo.vendorHash == lib.fakeHash;
assert !(baz ? cargoLock);
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
assert web.artifacts."fix.patch" == ./_sources/web/fix.patch;
assert baz.date == "2024-01-15";
runCommand "fetchrix-lib-test" { } "test -e ${webApp} && touch $out"
