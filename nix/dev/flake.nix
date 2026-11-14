{
  description = "fetchrix development inputs";

  inputs = {
    moraxyc-repo.url = "github:Moraxyc/pkgs.moraxyc.com";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "moraxyc-repo/nixpkgs";
    };
  };

  outputs = _: { };
}
