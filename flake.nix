{
  description = "whipper-gui: iced front end for whipper";

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
        xorg.libX11
        xorg.libXcursor
        xorg.libXi
        xorg.libXrandr
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
          whipper # so the buttons can actually spawn it
        ];
        buildInputs = runtimeLibs ++ [ pkgs.fontconfig ];

        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    };
}
