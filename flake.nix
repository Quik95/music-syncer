{
  description = "music-syncer: GPUI app";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      rust-overlay,
      git-hooks,
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

          runtimeLibs = with pkgs; [
            vulkan-loader
            libGL
            wayland
            libxkbcommon
          ];

          commonArgs = {
            src = craneLib.cleanCargoSource ./.;
            strictDeps = true;
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
            dontUseCmakeConfigure = true;
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          gitHooks = git-hooks.lib.${system}.run {
            src = ./.;
            hooks = {
              rustfmt = {
                enable = true;
                packageOverrides = {
                  cargo = toolchain;
                  rustfmt = toolchain;
                };
              };
              nixfmt.enable = true;
            };
          };

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
            system
            pkgs
            lib
            craneLib
            commonArgs
            cargoArtifacts
            gitHooks
            package
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
        pre-commit = env.gitHooks;
      });

      devShells = forAllSystems (env: {
        default = env.craneLib.devShell {
          checks = self.checks.${env.system};
          packages = [
            env.pkgs.just
            env.pkgs.cargo-nextest
          ]
          ++ env.gitHooks.enabledPackages;
          LD_LIBRARY_PATH = env.lib.makeLibraryPath env.runtimeLibs;
          inherit (env.gitHooks) shellHook;
        };
      });

      formatter = forAllSystems (
        env:
        let
          inherit (env.gitHooks.config) package configFile;
        in
        env.pkgs.writeShellScriptBin "pre-commit-run" ''
          ${env.lib.getExe package} run --all-files --config ${configFile}
        ''
      );
    };
}
