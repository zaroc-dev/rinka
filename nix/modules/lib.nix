{ self, lib, ... }:
{
  # Helpers for `services.rinka.projects`. They take the consumer's pkgs.
  flake.lib = rec {
    stemOf =
      file:
      let
        m = builtins.match "(.+)\\.[^.]*" file;
      in
      if m == null then file else builtins.head m;

    # An image project like the editor's import makes, from one image file.
    imageProject =
      pkgs:
      {
        image,
        title ? null,
        fit ? "cover",
      }:
      let
        file = baseNameOf (toString image);
        stem = stemOf file;
        ext = lib.toLower (lib.removePrefix stem file);
        words = lib.trim (builtins.replaceStrings [ "_" "-" "." ] [ " " " " " " ] stem);
        title' =
          if title != null then
            title
          else if words == "" then
            "Untitled"
          else
            lib.toUpper (builtins.substring 0 1 words) + builtins.substring 1 (-1) words;
        source = "assets/image${ext}";
        manifest = builtins.toJSON {
          format = "rinka.project";
          version = 1;
          title = title';
          content = {
            type = "image";
            inherit source fit;
            background = "#000000";
          };
        };
      in
      pkgs.runCommand "rinka-project-${lib.strings.sanitizeDerivationName stem}"
        {
          inherit manifest;
          passAsFile = [ "manifest" ];
        }
        ''
          mkdir -p $out/assets
          cp ${
            builtins.path {
              path = image;
              name = "image${ext}";
            }
          } $out/${source}
          cp $manifestPath $out/project.json
        '';

    # One image project per PNG or JPEG file in `dir`, named by the file's
    # stem: `imageProjects pkgs ./wallpapers` gives `{ moonlight = …; }`.
    imageProjects =
      pkgs: dir:
      let
        images = lib.filterAttrs (
          name: type: type == "regular" && builtins.match ".*\\.(png|jpe?g)" (lib.toLower name) != null
        ) (builtins.readDir dir);
      in
      lib.mapAttrs' (
        name: _:
        lib.nameValuePair (stemOf name) (
          imageProject pkgs {
            image = dir + "/${name}";
          }
        )
      ) images;

    inherit (import ../_service.nix { inherit self lib; }) projectId;
  };
}
