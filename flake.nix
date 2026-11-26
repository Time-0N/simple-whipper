{
  description = "simple-whipper: iced GUI CD ripper built on libcdio-paranoia";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux"; # change if needed
      pkgs = nixpkgs.legacyPackages.${system};

      # winit/wgpu dlopen these at runtime, so they must be on LD_LIBRARY_PATH
      runtimeLibs = with pkgs; [
        wayland
        libxkbcommon
        vulkan-loader
        libGL
        libx11
        libxcursor
        libxi
        libxrandr
      ];

      # C libraries we link against through FFI
      cdLibs = with pkgs; [
        libcdio
        libcdio-paranoia
      ];
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          cargo
          rustc
          rustfmt
          clippy
          rust-analyzer
          pkg-config
          xdg-utils # xdg-open for the "Open folder" button
        ];
        # sets LIBCLANG_PATH + BINDGEN_EXTRA_CLANG_ARGS (libc headers) for bindgen
        nativeBuildInputs = [ pkgs.rustPlatform.bindgenHook ];
        buildInputs = runtimeLibs ++ cdLibs ++ [ pkgs.fontconfig ];

        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs ++ cdLibs);
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    };
}
