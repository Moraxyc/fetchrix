{
  description = "Update package sources and derived hashes for Nix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    systems.url = "github:nix-systems/default";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
  };

  outputs =
    inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      systems = import inputs.systems;

      imports = [
        inputs.flake-parts.flakeModules.easyOverlay
        inputs.flake-parts.flakeModules.partitions
      ];

      partitions.dev = {
        module = ./nix/dev/flake-module.nix;
        extraInputsFlake = ./nix/dev;
      };
      partitionedAttrs = {
        checks = "dev";
        devShells = "dev";
        formatter = "dev";
      };

      flake.lib = import ./nix/lib.nix { inherit (inputs.nixpkgs) lib; };

      perSystem =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        {
          packages = {
            fetchrix = pkgs.callPackage ./nix/package.nix { };
            default = config.packages.fetchrix;
            fetchrix-static =
              if pkgs.stdenv.hostPlatform.isDarwin then
                (config.packages.fetchrix.override {
                  aws-lc = pkgs.aws-lc.override { useSharedLibraries = false; };
                }).overrideAttrs
                  (old: {
                    buildInputs = old.buildInputs ++ [ pkgs.pkgsStatic.libiconv ];
                    stripAllList = [ "bin" ];
                  })
              else
                (pkgs.pkgsStatic.callPackage ./nix/package.nix {
                  # The musl-targeting Go cannot link aws-lc's symbol reader or its cgo test runner.
                  aws-lc = (pkgs.pkgsStatic.aws-lc.override { inherit (pkgs) go; }).overrideAttrs {
                    doCheck = false;
                  };
                }).overrideAttrs
                  {
                    doCheck = false;
                    stripAllList = [ "bin" ];
                  };
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            fetchrix-deb = pkgs.callPackage ./nix/deb.nix { inherit (config.packages) fetchrix-static; };
          };

          overlayAttrs = { inherit (config.packages) fetchrix; };

          checks = {
            package = config.packages.default;
            lib = pkgs.callPackage ./nix/tests/lib.nix { fetchrixLib = inputs.self.lib; };
            clippy = config.packages.default.overrideAttrs (old: {
              pname = "${old.pname}-clippy";
              nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
              buildPhase = ''
                runHook preBuild
                cargo clippy --all-targets --offline -- -D warnings
                runHook postBuild
              '';
              doCheck = false;
              installPhase = "touch $out";
            });
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            deb = config.packages.fetchrix-deb;
          };
        };
    };
}
