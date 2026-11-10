{
  perSystem =
    { pkgs, ... }:
    let
      rinkaLib = import ../_lib.nix { inherit pkgs; };
    in
    {
      devShells.default = pkgs.mkShell {
        # Sets LIBCLANG_PATH for bindgen (ffmpeg-sys-next).
        nativeBuildInputs = [ pkgs.rustPlatform.bindgenHook ];
        packages = with pkgs; [
          rinkaLib.rustToolchain
          # The same CLI the package build uses, matched to Cargo.lock.
          rinkaLib.packages.wasm-bindgen-cli
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
        buildInputs = rinkaLib.runtimeLibs ++ [ pkgs.ffmpeg ];
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath rinkaLib.runtimeLibs;
        # Electron comes from nixpkgs; the npm package only provides types.
        ELECTRON_SKIP_BINARY_DOWNLOAD = "1";
        ELECTRON_OVERRIDE_DIST_PATH = "${pkgs.electron}/bin";
        # Type-checking the Windows host from Linux: bindgen must parse the
        # FFmpeg headers against mingw-w64's C headers, not glibc's. winnt.h
        # pulls in clang's x86intrin.h, whose AMX types Rust rejects.
        shellHook = ''
          clang_resource=$(grep -o -- '-resource-dir=[^ ]*' <<< "$BINDGEN_EXTRA_CLANG_ARGS" | cut -d= -f2)
          export BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_msvc="--target=x86_64-pc-windows-gnu -resource-dir=$clang_resource -nostdlibinc -isystem $clang_resource/include -isystem ${pkgs.pkgsCross.mingwW64.windows.mingw_w64_headers}/include -D__X86INTRIN_H"
        '';
      };

      formatter = pkgs.nixfmt;
    };
}
