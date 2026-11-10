{ inputs, self, ... }:
{
  perSystem =
    { system, pkgs, ... }:
    {
      # The package builds and the dev shell need rust-overlay's toolchains.
      _module.args.pkgs = import inputs.nixpkgs {
        inherit system;
        overlays = [ inputs.rust-overlay.overlays.default ];
      };
      packages = (import ../_lib.nix { inherit pkgs; }).packages;
    };

  flake.overlays.default = final: prev: {
    inherit (self.packages.${final.stdenv.hostPlatform.system}) rinka rinka-host;
  };
}
