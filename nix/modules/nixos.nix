{ self, ... }:
let
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
        package = lib.mkOption {
          type = lib.types.package;
          # rinka's own package set: the build needs rust-overlay, which the
          # consumer's pkgs usually lacks.
          default = self.packages.${pkgs.stdenv.hostPlatform.system}.rinka;
          defaultText = lib.literalExpression "rinka.packages.\${system}.rinka";
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
        environment.systemPackages = [ cfg.package ];
        systemd.user.services.rinka-host = {
          description = "Rinka wallpaper host";
          partOf = [ "graphical-session.target" ];
          after = [ "graphical-session.target" ];
          wantedBy = [ "graphical-session.target" ];
          # `|` makes the conditions an OR instead of an AND.
          unitConfig.ConditionEnvironment = map (d: "|XDG_CURRENT_DESKTOP=${d}") cfg.desktops;
          serviceConfig = {
            ExecStart = "${cfg.package}/bin/rinka-host serve --restore";
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
