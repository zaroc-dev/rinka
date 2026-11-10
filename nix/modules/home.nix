{ self, lib, ... }:
let
  service = import ../_service.nix { inherit self lib; };

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
      }
      // service.options pkgs;
      config = lib.mkIf cfg.enable {
        assertions = service.assertions cfg;
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
            ExecStartPre = lib.mkIf (cfg.projects != { }) (service.execStartPre pkgs cfg);
            ExecStart = service.execStart cfg;
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
