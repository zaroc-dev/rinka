{
  description = "Rinka: a native wallpaper engine and editor for Wayland";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );

      # Libraries loaded at runtime (dlopen) by wgpu and the Wayland host.
      runtimeLibs =
        pkgs: with pkgs; [
          vulkan-loader
          wayland
          libxkbcommon
          libGL
        ];

      rustToolchain =
        pkgs:
        pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
          targets = [
            "wasm32-unknown-unknown"
            # Type-checking the Windows host from Linux.
            "x86_64-pc-windows-msvc"
          ];
        };

      packagesFor =
        pkgs:
        let
          inherit (pkgs) lib;
          version = (lib.importTOML ./Cargo.toml).workspace.package.version;
          rust = rustToolchain pkgs;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rust;
            rustc = rust;
          };
          rustSrc = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./.cargo
              ./crates
            ];
          };

          rinka-host = rustPlatform.buildRustPackage {
            pname = "rinka-host";
            inherit version;
            src = rustSrc;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "-p"
              "rinka-host"
            ];
            nativeBuildInputs = [
              pkgs.pkg-config
              rustPlatform.bindgenHook
              pkgs.makeWrapper
            ];
            buildInputs = [ pkgs.ffmpeg ] ++ runtimeLibs pkgs;
            # The tests render on a GPU, which the build sandbox lacks.
            doCheck = false;
            postFixup = ''
              wrapProgram $out/bin/rinka-host \
                --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath (runtimeLibs pkgs)}
            '';
            meta = {
              description = "Rinka wallpaper host: draws wallpapers on Wayland outputs";
              license = lib.licenses.gpl3Plus;
              platforms = lib.platforms.linux;
              mainProgram = "rinka-host";
            };
          };

          # The engine compiled to WebAssembly for the editor's live preview.
          rinka-wasm = pkgs.stdenv.mkDerivation {
            pname = "rinka-wasm";
            inherit version;
            src = rustSrc;
            cargoDeps = rustPlatform.importCargoLock { lockFile = ./Cargo.lock; };
            nativeBuildInputs = [
              rust
              rustPlatform.cargoSetupHook
              pkgs.wasm-bindgen-cli
            ];
            buildPhase = ''
              runHook preBuild
              cargo build --offline -p rinka-wasm --target wasm32-unknown-unknown --profile release-wasm
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              wasm-bindgen --target web --out-dir $out \
                target/wasm32-unknown-unknown/release-wasm/rinka_wasm.wasm
              runHook postInstall
            '';
          };

          rinka = pkgs.stdenv.mkDerivation (finalAttrs: {
            pname = "rinka";
            inherit version;
            src = ./app;
            pnpmDeps = pkgs.fetchPnpmDeps {
              inherit (finalAttrs) pname version src;
              inherit (pkgs) pnpm;
              fetcherVersion = 4;
              hash = "sha256-4NZzQTp17HnjeYYvh2roW7xBcYncPR/PeFiCEe/M270=";
            };
            nativeBuildInputs = [
              pkgs.nodejs
              pkgs.pnpm
              pkgs.pnpmConfigHook
              pkgs.makeWrapper
              pkgs.copyDesktopItems
            ];
            env.ELECTRON_SKIP_BINARY_DOWNLOAD = "1";
            buildPhase = ''
              runHook preBuild
              cp -r ${rinka-wasm} src/renderer/wasm
              chmod -R u+w src/renderer/wasm
              node scripts/build.mjs
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              mkdir -p $out/share/rinka/app $out/bin
              cp -r package.json dist $out/share/rinka/app/
              makeWrapper ${pkgs.electron}/bin/electron $out/bin/rinka \
                --add-flags $out/share/rinka/app \
                --set-default RINKA_HOST_BIN ${rinka-host}/bin/rinka-host
              ln -s ${rinka-host}/bin/rinka-host $out/bin/rinka-host
              install -Dm644 ${./assets/rinka.svg} $out/share/icons/hicolor/scalable/apps/rinka.svg
              runHook postInstall
            '';
            desktopItems = [
              (pkgs.makeDesktopItem {
                name = "rinka";
                desktopName = "Rinka";
                genericName = "Wallpaper engine";
                comment = "Animated wallpapers, images, videos and Wallpaper Engine scenes";
                exec = "rinka %U";
                icon = "rinka";
                categories = [
                  "Graphics"
                  "Utility"
                ];
              })
            ];
            meta = {
              description = "Native wallpaper engine and editor for Wayland";
              homepage = "https://github.com/zaroc-dev/rinka";
              license = lib.licenses.gpl3Plus;
              platforms = lib.platforms.linux;
              mainProgram = "rinka";
            };
          });
        in
        {
          inherit rinka rinka-host rinka-wasm;
          default = rinka;
        };
    in
    {
      packages = forAllSystems packagesFor;

      overlays.default = final: prev: {
        inherit (self.packages.${final.stdenv.hostPlatform.system}) rinka rinka-host;
      };

      # Runs the wallpaper host at login and restores the wallpapers chosen in
      # the editor.
      homeManagerModules.default =
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
          };
          config = lib.mkIf cfg.enable {
            home.packages = [ cfg.package ];
            systemd.user.services.rinka-host = {
              Unit = {
                Description = "Rinka wallpaper host";
                PartOf = [ "graphical-session.target" ];
                After = [ "graphical-session.target" ];
              };
              Service = {
                ExecStart = "${cfg.package}/bin/rinka-host serve --restore";
                Restart = "on-failure";
              };
              Install.WantedBy = [ "graphical-session.target" ];
            };
          };
        };

      devShells = forAllSystems (
        pkgs:
        {
          default = pkgs.mkShell {
            # Sets LIBCLANG_PATH for bindgen (ffmpeg-sys-next).
            nativeBuildInputs = [ pkgs.rustPlatform.bindgenHook ];
            packages = with pkgs; [
              (rustToolchain pkgs)
              wasm-bindgen-cli
              binaryen
              pkg-config
              nodejs
              pnpm
              electron
              vulkan-tools
              wayland-utils
              ffmpeg
            ];
            # FFmpeg libraries for native video decoding (ffmpeg-next).
            buildInputs = runtimeLibs pkgs ++ [ pkgs.ffmpeg ];
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
            # Electron comes from nixpkgs; the npm package only provides types.
            ELECTRON_SKIP_BINARY_DOWNLOAD = "1";
            ELECTRON_OVERRIDE_DIST_PATH = "${pkgs.electron}/bin";
          };
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
