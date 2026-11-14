{ inputs, ... }:
{
  imports = [ inputs.treefmt-nix.flakeModule ];

  perSystem =
    { config, pkgs, ... }:
    {
      treefmt = {
        projectRootFile = "flake.nix";
        programs = {
          rustfmt.enable = true;
          nixfmt.enable = true;
          taplo.enable = true;
        };
      };

      devShells.default = pkgs.mkShell {
        inputsFrom = [ config.packages.default ];
        packages = [
          pkgs.clippy
          pkgs.git-cliff
          pkgs.rust-analyzer
          pkgs.rustfmt
        ];
      };
    };
}
