# fetchrix

Update package sources and derived hashes for Nix.

## Usage

`fetchrix update` writes `_sources/lock.json` and extracted lock files under
`_sources/<package>/src/`. Expose them through an overlay:

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

An `npm-deps` lock file yields `importNpmLock` arguments. Local and workspace
dependencies resolve against the fetched `src`, so only `package.json` and
`package-lock.json` are extracted.

`fetchrix.lib.load { pkgs; dir; }` returns the same set without an overlay.

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
