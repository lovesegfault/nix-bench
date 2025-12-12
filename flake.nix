{
  description = "nix-bench - benchmarking library for Nix build systems";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";

    crane.url = "github:ipetkov/crane";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, nixpkgs, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } (
      { self, ... }:
      {
        debug = true;
        imports = [
          inputs.git-hooks.flakeModule
          inputs.treefmt-nix.flakeModule
        ];

        systems = [
          "x86_64-linux"
          "aarch64-linux"
        ];

        # Expose lib at flake level for external consumers
        flake.lib = import ./lib.nix;

        perSystem =
          {
            config,
            pkgs,
            system,
            ...
          }:
          let
            nixBench = self.lib.mkNixBench { inherit pkgs; };

            # Rust toolchain
            rustToolchain = pkgs.rust-bin.stable.latest.default.override {
              extensions = [
                "rust-src"
                "rust-analyzer"
              ];
              targets = [
                "x86_64-unknown-linux-gnu"
                "aarch64-unknown-linux-gnu"
              ];
            };

            craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;

            # Common build inputs for Rust crates
            src = craneLib.cleanCargoSource ./crates;

            commonArgs = {
              inherit src;
              strictDeps = true;
              pname = "nix-bench";
              version = "0.1.0";
              buildInputs = with pkgs; [
                openssl
                sqlite
              ];
              nativeBuildInputs = with pkgs; [
                pkg-config
              ];
            };

            # Build dependencies (for caching)
            cargoArtifacts = craneLib.buildDepsOnly commonArgs;

            # Individual crates
            nix-bench-ec2 = craneLib.buildPackage (
              commonArgs
              // {
                inherit cargoArtifacts;
                pname = "nix-bench-ec2";
                cargoExtraArgs = "-p nix-bench-ec2";
              }
            );

            nix-bench-agent = craneLib.buildPackage (
              commonArgs
              // {
                inherit cargoArtifacts;
                pname = "nix-bench-agent";
                cargoExtraArgs = "-p nix-bench-agent";
              }
            );
          in
          {
            _module.args.pkgs = import nixpkgs {
              inherit system;
              config.allowUnfree = true;
              overlays = [ inputs.rust-overlay.overlays.default ];
            };

            # Tiered benchmark packages + Rust binaries
            packages = nixBench.mkTieredPackages { } // {
              inherit nix-bench-ec2 nix-bench-agent;
              default = nix-bench-ec2;
            };

            # Magic attrsets: .#shallow.hello, .#deep.firefox
            legacyPackages = {
              shallow = nixBench.shallowPkgs;
              deep = nixBench.deepPkgs;
            };

            # Treefmt configuration
            treefmt = {
              projectRootFile = "flake.nix";
              flakeCheck = false; # Covered by git-hooks check
              programs.nixfmt.enable = true;
            };

            # Pre-commit hooks
            pre-commit = {
              check.enable = true;
              settings.hooks = {
                check-added-large-files.enable = true;
                check-merge-conflicts.enable = true;
                check-symlinks.enable = true;
                convco.enable = true;
                deadnix.enable = true;
                end-of-file-fixer.enable = true;
                nil.enable = true;
                ripsecrets.enable = true;
                statix.enable = true;
                treefmt.enable = true;
                trim-trailing-whitespace.enable = true;
              };
            };

            # Development shell
            devShells.default = pkgs.mkShell {
              name = "nix-bench";
              nativeBuildInputs =
                with pkgs;
                [
                  nix-eval-jobs

                  # Rust development
                  rustToolchain
                  pkg-config
                  openssl
                  sqlite
                  cargo-nextest

                  # AWS CLI for testing
                  awscli2
                ]
                ++ [ config.treefmt.build.wrapper ]
                ++ (builtins.attrValues config.treefmt.build.programs)
                ++ config.pre-commit.settings.enabledPackages;
              shellHook = ''
                ${config.pre-commit.installationScript}
              '';

              RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
            };
          };
      }
    );
}
