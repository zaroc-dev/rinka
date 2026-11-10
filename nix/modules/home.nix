{ self, lib, ... }:
let
  # Runs the wallpaper host at login and restores the wallpapers chosen in the
  # editor.
  module =
    {
      config,
      lib,
      pkgs,
      ...
    }:
    let
      cfg = config.services.rinka;
    in
    {
      options.services.rinka = {
        enable = lib.mkEnableOption "Rinka, the wallpaper engine";
        package = lib.mkOption {
          type = lib.types.package;
          default = self.packages.${pkgs.stdenv.hostPlatform.system}.rinka;
          description = "The Rinka package (editor and host).";
        };
        desktops = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          default = [ ];
          example = [ "niri" ];
          description = ''
            Start the host only in sessions whose XDG_CURRENT_DESKTOP is one of
            these. Empty starts it in every graphical session.
          '';
        };
      };
      config = lib.mkIf cfg.enable {
        home.packages = [ cfg.package ];
        systemd.user.services.rinka-host = {
          Unit = {
            Description = "Rinka wallpaper host";
            PartOf = [ "graphical-session.target" ];
            After = [ "graphical-session.target" ];
            # `|` makes the conditions an OR instead of an AND.
            ConditionEnvironment = lib.mkIf (cfg.desktops != [ ]) (
              map (d: "|XDG_CURRENT_DESKTOP=${d}") cfg.desktops
            );
          };
          Service = {
            ExecStart = "${cfg.package}/bin/rinka-host serve --restore";
            Restart = "on-failure";
          };
          Install.WantedBy = [ "graphical-session.target" ];
        };
      };
    };
in
{
  # flake-parts declares nixosModules but not homeModules.
  options.flake.homeModules = lib.mkOption {
    type = lib.types.lazyAttrsOf lib.types.raw;
    default = { };
    description = "home-manager modules exposed by this flake.";
  };

  config.flake = {
    homeModules = {
      rinka = module;
      default = module;
    };
    # The pre-flake-parts name, kept so existing configurations keep evaluating.
    homeManagerModules.default = module;
  };
}
