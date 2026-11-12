# fetchrix

Update package sources and derived hashes for Nix.

## Usage

`fetchrix update` writes `_sources/lock.json` and extracted lock files under
`_sources/<package>/src/`.

### With flakes

Add fetchrix as a flake input and use `fetchrix.lib.overlay` or
`fetchrix.lib.load`:

```nix
overlays.default = lib.composeManyExtensions [
  (fetchrix.lib.overlay { dir = ./_sources; }) # attr defaults to "sources"
  (final: prev: {
    foo = final.rustPlatform.buildRustPackage {
      pname = "foo";
      inherit (final.sources.foo) version src cargoLock;
    };
    bar = final.buildNpmPackage {
      pname = "bar";
      inherit (final.sources.bar) version src;
      npmDeps = final.importNpmLock final.sources.bar.npmDeps;
      npmConfigHook = final.importNpmLock.npmConfigHook;
    };
  })
];
```

### Without flakes

**overlay.nix** — apply via `nixpkgs.overlays` or `pkgs.extend`:

```nix
# in your nixos configuration or pkgs instantiation
nixpkgs.overlays = [
  (import ./fetchrix/overlay.nix)          # adds pkgs.fetchrix
  (import (fetchrixSrc + "/nix/lib.nix") { inherit (pkgs) lib; }).overlay
    { dir = ./_sources; }                  # adds pkgs.sources
];
```

**default.nix** — build fetchrix itself:

```sh
nix-build /path/to/fetchrix
```

**lib helper** — load sources without an overlay:

```nix
let
  fetchrixLib = import ./fetchrix/nix/lib.nix { inherit (pkgs) lib; };
  sources = fetchrixLib.load { inherit pkgs; dir = ./_sources; };
in
{
  foo = pkgs.rustPlatform.buildRustPackage {
    pname = "foo";
    inherit (sources.foo) version src cargoLock;
  };
}
```

An `npm-deps` lock file yields `importNpmLock` arguments. Local and workspace
dependencies resolve against the fetched `src`, so only `package.json` and
`package-lock.json` are extracted.

## Development

```sh
nix develop       # toolchain
nix fmt           # format
nix flake check   # build, tests, clippy, formatting
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

## Credits

Inspired by [nvfetcher](https://github.com/berberman/nvfetcher).
