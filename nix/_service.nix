# What the NixOS and home-manager modules share: the options of
# `services.rinka` (besides `enable`) and the host's start commands.
{ self, lib }:
rec {
  # The editor's library only accepts UUIDs as project ids, so each name in
  # `services.rinka.projects` maps to a stable UUID derived from it.
  projectId =
    name:
    let
      h = builtins.hashString "sha256" "rinka-project:${name}";
      s = start: len: builtins.substring start len h;
    in
    "${s 0 8}-${s 8 4}-5${s 13 3}-8${s 17 3}-${s 20 12}";

  options = pkgs: {
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
    projects = lib.mkOption {
      type = lib.types.attrsOf lib.types.path;
      default = { };
      example = lib.literalExpression ''
        rinka.lib.imageProjects pkgs ./wallpapers // { moonlight = ./rinka/moonlight; }
      '';
      description = ''
        Rinka projects (directories holding a project.json) to add to each
        user's library when the host starts. Each is copied once, as an
        editable project: edits are kept, and a project deleted in the editor
        is not added again. `rinka.lib.imageProject` and
        `rinka.lib.imageProjects` make projects from image files.
      '';
    };
    fallback = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "moonlight";
      description = ''
        Name of an entry in `projects` to show on every output that has no
        wallpaper assigned in the editor, for example on first login or on
        a newly connected monitor.
      '';
    };
  };

  assertions = cfg: [
    {
      assertion = cfg.fallback == null || cfg.projects ? ${cfg.fallback};
      message = "services.rinka.fallback (${toString cfg.fallback}) must name an entry in services.rinka.projects.";
    }
  ];

  # Copies new projects into the library. Seeded ids are remembered in the
  # state directory so a project the user deleted stays deleted.
  seed =
    pkgs: cfg:
    pkgs.writeShellScript "rinka-seed-library" ''
      set -eu
      library="''${XDG_CONFIG_HOME:-$HOME/.config}/rinka/projects"
      state="''${XDG_STATE_HOME:-$HOME/.local/state}/rinka"
      ${pkgs.coreutils}/bin/mkdir -p "$library" "$state"
      ${pkgs.coreutils}/bin/touch "$state/seeded"
      seed() {
        if ${pkgs.gnugrep}/bin/grep -qxF "$1" "$state/seeded"; then return; fi
        if [ ! -e "$library/$1" ]; then
          ${pkgs.coreutils}/bin/rm -rf "$library/.$1.tmp"
          ${pkgs.coreutils}/bin/cp -rT --no-preserve=mode,ownership "$2" "$library/.$1.tmp"
          ${pkgs.coreutils}/bin/mv -T "$library/.$1.tmp" "$library/$1"
        fi
        echo "$1" >> "$state/seeded"
      }
      ${lib.concatStrings (
        lib.mapAttrsToList (name: src: ''
          seed ${projectId name} ${lib.escapeShellArg "${src}"}
        '') cfg.projects
      )}
    '';

  execStartPre = pkgs: cfg: lib.optional (cfg.projects != { }) "${seed pkgs cfg}";

  execStart =
    cfg:
    "${cfg.package}/bin/rinka-host serve --restore"
    + lib.optionalString (cfg.fallback != null) " --fallback ${projectId cfg.fallback}";
}
