# Shared by the flake-parts modules under nix/modules. The leading underscore
# keeps import-tree from loading this file as a module.
{ pkgs }:
let
  # Libraries loaded at runtime (dlopen) by wgpu and the Wayland host.
  runtimeLibs =
    pkgs: with pkgs; [
      vulkan-loader
      # Sound output (cpal opens ALSA; PipeWire serves it on most desktops).
      alsa-lib
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
      version = (lib.importTOML ../Cargo.toml).workspace.package.version;
      rust = rustToolchain pkgs;
      rustPlatform = pkgs.makeRustPlatform {
        cargo = rust;
        rustc = rust;
      };
      rustSrc = lib.fileset.toSource {
        root = ../.;
        fileset = lib.fileset.unions [
          ../Cargo.toml
          ../Cargo.lock
          ../.cargo
          ../crates
        ];
      };

      rinka-host = rustPlatform.buildRustPackage {
        pname = "rinka-host";
        inherit version;
        src = rustSrc;
        cargoLock.lockFile = ../Cargo.lock;
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
        # KDE Plasma's "Rinka" wallpaper type (see src/plasma.rs).
        postInstall = ''
          mkdir -p $out/share/plasma/wallpapers
          cp -r crates/rinka-host/plasma/wallpaper $out/share/plasma/wallpapers/dev.rinka.wallpaper
        '';
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

      # wasm-bindgen's CLI must match the crate in Cargo.lock exactly. Build
      # it here rather than taking nixpkgs' copy: consumers that make this
      # flake follow their own nixpkgs would otherwise get a different CLI
      # version and a failing build. Bump the hashes with the crate.
      wasmBindgenVersion =
        (lib.findFirst (p: p.name == "wasm-bindgen") null (lib.importTOML ../Cargo.lock).package).version;
      wasm-bindgen-cli = pkgs.buildWasmBindgenCli rec {
        src = pkgs.fetchCrate {
          pname = "wasm-bindgen-cli";
          version = wasmBindgenVersion;
          hash = "sha256-pcecKQd7E8Opw6bkFoE569epUi7gh5qpQF1e5PJY6V8=";
        };
        cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
          inherit src;
          inherit (src) pname version;
          hash = "sha256-vmUrWVU7kPJJxO5qIVeAkwQyWDELO1Z4Z5gitz2kco8=";
        };
      };

      # The engine compiled to WebAssembly for the editor's live preview.
      rinka-wasm = pkgs.stdenv.mkDerivation {
        pname = "rinka-wasm";
        inherit version;
        src = rustSrc;
        cargoDeps = rustPlatform.importCargoLock { lockFile = ../Cargo.lock; };
        nativeBuildInputs = [
          rust
          rustPlatform.cargoSetupHook
          wasm-bindgen-cli
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
        src = ../app;
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
          mkdir -p $out/share/plasma
          ln -s ${rinka-host}/share/plasma/wallpapers $out/share/plasma/wallpapers
          install -Dm644 ${../assets/rinka.svg} $out/share/icons/hicolor/scalable/apps/rinka.svg
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
      inherit
        rinka
        rinka-host
        rinka-wasm
        wasm-bindgen-cli
        ;
      default = rinka;
    };
in
{
  runtimeLibs = runtimeLibs pkgs;
  rustToolchain = rustToolchain pkgs;
  packages = packagesFor pkgs;
}
