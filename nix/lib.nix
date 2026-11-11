{ lib }:
rec {
  /**
    Reads the fetchrix output directory `dir` into
    `{ <name> = { version; src; <derived>...; }; }`.

    A derived hash becomes its SRI string. An extracted lock file becomes
    `{ lockFile; outputHashes; }`, which `cargoLock` accepts directly.
  */
  load =
    { pkgs, dir }:
    let
      parsed = lib.importJSON (dir + "/lock.json");

      fetchSrc =
        src:
        let
          args = removeAttrs src [ "fetcher" ];
        in
        {
          url = pkgs.fetchurl args;
          tarball = pkgs.fetchzip args;
          github = pkgs.fetchFromGitHub args;
          # fetchgit fetches submodules by default; the lock omits `false`.
          git = pkgs.fetchgit ({ fetchSubmodules = false; } // args);
        }
        .${src.fetcher} or (throw "fetchrix: unknown fetcher `${src.fetcher}`");

      derive =
        entry:
        if entry ? hash then
          entry.hash
        else
          {
            lockFile = dir + "/${entry.lockfile}";
            outputHashes = entry.outputHashes or { };
          };
    in
    assert lib.assertMsg (
      parsed.schema == 1
    ) "fetchrix: unsupported lock schema ${toString parsed.schema}";
    lib.mapAttrs (
      _: pkg:
      {
        inherit (pkg) version;
        src = fetchSrc pkg.src;
      }
      // lib.mapAttrs (_: derive) (pkg.derived or { })
    ) parsed.packages;

  /**
    Overlay that exposes `load { dir; }` as `pkgs.<attr>`, using the
    fetchers of the final package set.
  */
  overlay =
    {
      dir,
      attr ? "sources",
    }:
    final: _prev: {
      ${attr} = load {
        pkgs = final;
        inherit dir;
      };
    };
}
