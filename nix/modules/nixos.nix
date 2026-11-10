{ self, lib, ... }:
let
  service = import ../_service.nix { inherit self lib; };

  # Installs Rinka for every user and runs the wallpaper host in their
  # graphical sessions, restoring the wallpapers chosen in the editor.
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
        enable = lib.mkEnableOption "Rinka, the wallpaper engine, for every user";
      }
      // service.options pkgs;

      config = lib.mkIf cfg.enable {
        assertions = service.assertions cfg;
        environment.systemPackages = [ cfg.package ];
        systemd.user.services.rinka-host = {
          description = "Rinka wallpaper host";
          partOf = [ "graphical-session.target" ];
          after = [ "graphical-session.target" ];
          wantedBy = [ "graphical-session.target" ];
          # `|` makes the conditions an OR instead of an AND.
          unitConfig.ConditionEnvironment = map (d: "|XDG_CURRENT_DESKTOP=${d}") cfg.desktops;
          serviceConfig = {
            ExecStartPre = service.execStartPre pkgs cfg;
            ExecStart = service.execStart cfg;
            Restart = "on-failure";
          };
        };
      };
    };
in
{
  flake.nixosModules = {
    rinka = module;
    default = module;
  };
}
