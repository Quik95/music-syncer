{
  description = "music-syncer: GPUI app";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      crane,
      rust-overlay,
      ...
    }:
    let
      systems = [ "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (mkEnv system));

      mkEnv =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          inherit (pkgs) lib;

          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain (_: toolchain);

          nativeBuildInputs = with pkgs; [
            pkg-config
            cmake
          ];

          buildInputs = with pkgs; [
            fontconfig
            freetype
            openssl
            libxkbcommon
            wayland
          ];

          runtimeLibs = with pkgs; [
            vulkan-loader
            libGL
            wayland
            libxkbcommon
          ];

          commonArgs = {
            src = craneLib.cleanCargoSource ./.;
            strictDeps = true;
            inherit nativeBuildInputs buildInputs;
            dontUseCmakeConfigure = true;
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          package = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              postFixup = ''
                patchelf $out/bin/music-syncer --add-rpath ${lib.makeLibraryPath runtimeLibs}
              '';
              meta = {
                mainProgram = "music-syncer";
                license = lib.licenses.eupl12;
              };
            }
          );
        in
        {
          inherit
            pkgs
            lib
            craneLib
            commonArgs
            cargoArtifacts
            package
            buildInputs
            nativeBuildInputs
            runtimeLibs
            ;
        };
    in
    {
      packages = forAllSystems (env: {
        default = env.package;
      });

      checks = forAllSystems (env: {
        inherit (env) package;
        clippy = env.craneLib.cargoClippy (
          env.commonArgs
          // {
            inherit (env) cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          }
        );
        nextest = env.craneLib.cargoNextest (
          env.commonArgs
          // {
            inherit (env) cargoArtifacts;
            cargoNextestExtraArgs = "--no-tests=warn";
          }
        );
        fmt = env.craneLib.cargoFmt { src = env.commonArgs.src; };
      });

      devShells = forAllSystems (env: {
        default = env.craneLib.devShell {
          packages =
            env.nativeBuildInputs
            ++ env.buildInputs
            ++ [
              env.pkgs.just
              env.pkgs.cargo-nextest
            ];
          LD_LIBRARY_PATH = env.lib.makeLibraryPath env.runtimeLibs;
        };
      });

      formatter = forAllSystems (env: env.pkgs.nixfmt);
    };
}
