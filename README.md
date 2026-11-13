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
      inherit (final.sources.foo) pname version src cargoLock;
    };
    bar = final.buildNpmPackage {
      inherit (final.sources.bar) pname version src;
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
    inherit (sources.foo) pname version src cargoLock;
  };
}
```

An `npm-deps` lock file yields `importNpmLock` arguments. Local and workspace
dependencies resolve against the fetched `src`, so only `package.json` and
`package-lock.json` are extracted.

### Preparing the source

`prepare` runs steps in a writable copy of the fetched source. Derived outputs
are computed from that copy, and declared artifacts are written to
`_sources/<package>/<name>` and exposed as `sources.<package>.artifacts.<name>`.

```toml
[packages.shlink.prepare]
steps = [{ type = "composer-lock" }]
artifacts."composer.lock" = { type = "file", path = "composer.lock" }

[packages.foo.prepare]
steps = [
  { type = "npm-lock", args = ["--legacy-peer-deps"] },
  { type = "hook", run = ["./scripts/patch-foo.sh"] },
]
artifacts."fix.patch" = { type = "patch", paths = ["package.json", "package-lock.json"] }
```

`npm-lock` and `composer-lock` take tools from `[nix].nixpkgs`. A `hook`
runs a program given by a path relative to the configuration file, with
`FETCHRIX_SOURCE` and `FETCHRIX_VERSION` set. A `patch` artifact holds
the changes made by the steps, applicable with `patches`.

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
