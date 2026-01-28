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

            # Read version from workspace Cargo.toml (single source of truth)
            cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
            inherit (cargoToml.workspace.package) version;

            # Rust toolchain builder function (for cross-compilation support)
            mkRustToolchain =
              p:
              p.rust-bin.stable.latest.default.override {
                extensions = [
                  "rust-src"
                  "rust-analyzer"
                ];
                targets = [
                  "x86_64-unknown-linux-gnu"
                  "x86_64-unknown-linux-musl"
                  "aarch64-unknown-linux-gnu"
                  "aarch64-unknown-linux-musl"
                ];
              };

            # Pre-built toolchain for native builds and devShell
            rustToolchain = mkRustToolchain pkgs;

            craneLib = (inputs.crane.mkLib pkgs).overrideToolchain mkRustToolchain;

            # Common build inputs for Rust crate
            src = craneLib.cleanCargoSource ./.;

            commonArgs = {
              inherit src;
              strictDeps = true;
              pname = "nix-bench-workspace";
              inherit version;
              buildInputs = with pkgs; [
                openssl
              ];
              nativeBuildInputs = with pkgs; [
                pkg-config
              ];
            };

            # Build dependencies (for caching) - builds all workspace crates' deps
            cargoArtifacts = craneLib.buildDepsOnly commonArgs;

            # Test with nextest using nix profile (skips AWS integration tests)
            nix-bench-nextest = craneLib.cargoNextest (
              commonArgs
              // {
                inherit cargoArtifacts;
                nativeBuildInputs = (commonArgs.nativeBuildInputs or [ ]) ++ [ pkgs.cargo-nextest ];
                cargoNextestExtraArgs = "--profile nix";
              }
            );

            # Coordinator binary (nix-bench-coordinator)
            nix-bench-coordinator = craneLib.buildPackage (
              commonArgs
              // {
                inherit cargoArtifacts;
                pname = "nix-bench-coordinator";
                cargoExtraArgs = "-p nix-bench-coordinator";
                # Skip default cargo test, we use nextest separately
                doCheck = false;
              }
            );

            # Agent binary (nix-bench-agent)
            nix-bench-agent = craneLib.buildPackage (
              commonArgs
              // {
                inherit cargoArtifacts;
                pname = "nix-bench-agent";
                cargoExtraArgs = "-p nix-bench-agent";
                # Skip default cargo test, we use nextest separately
                doCheck = false;
              }
            );

            # Cross-compilation for aarch64 with musl (static linking, only on x86_64)
            nix-bench-agent-aarch64 =
              if system == "x86_64-linux" then
                let
                  crossPkgs = import nixpkgs {
                    inherit system;
                    crossSystem.config = "aarch64-unknown-linux-musl";
                    overlays = [ inputs.rust-overlay.overlays.default ];
                  };
                  crossCraneLib = (inputs.crane.mkLib crossPkgs).overrideToolchain mkRustToolchain;
                  crossArgs = {
                    inherit src;
                    strictDeps = true;
                    pname = "nix-bench-agent-aarch64";
                    inherit version;
                    CARGO_BUILD_TARGET = "aarch64-unknown-linux-musl";
                    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER = "${crossPkgs.stdenv.cc.targetPrefix}cc";
                    # Build static binary
                    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS = "-C target-feature=+crt-static";
                    HOST_CC = "${pkgs.stdenv.cc.nativePrefix}cc";
                    buildInputs = with crossPkgs; [
                      openssl.dev
                    ];
                    nativeBuildInputs = with pkgs; [
                      pkg-config
                    ];
                    depsBuildBuild = with pkgs; [
                      stdenv.cc
                    ];
                    # Use static OpenSSL
                    OPENSSL_STATIC = "1";
                    OPENSSL_LIB_DIR = "${crossPkgs.openssl.out}/lib";
                    OPENSSL_INCLUDE_DIR = "${crossPkgs.openssl.dev}/include";
                  };
                in
                crossCraneLib.buildPackage (
                  crossArgs
                  // {
                    cargoArtifacts = crossCraneLib.buildDepsOnly crossArgs;
                    cargoExtraArgs = "-p nix-bench-agent";
                  }
                )
              else
                null;

            # Combined package with coordinator + agents
            nixBenchCombined = pkgs.symlinkJoin {
              name = "nix-bench";
              paths = [ nix-bench-coordinator ];
              nativeBuildInputs = [ pkgs.makeWrapper ];
              postBuild = ''
                # Create lib directory for agent binaries
                mkdir -p $out/lib/nix-bench
                cp ${nix-bench-agent}/bin/nix-bench-agent $out/lib/nix-bench/agent-x86_64
                ${
                  if nix-bench-agent-aarch64 != null then
                    "cp ${nix-bench-agent-aarch64}/bin/nix-bench-agent $out/lib/nix-bench/agent-aarch64"
                  else
                    ""
                }

                # Wrap the binary to set default agent paths
                wrapProgram $out/bin/nix-bench-coordinator \
                  --set NIX_BENCH_AGENT_X86_64 "$out/lib/nix-bench/agent-x86_64" \
                  ${
                    if nix-bench-agent-aarch64 != null then
                      ''--set NIX_BENCH_AGENT_AARCH64 "$out/lib/nix-bench/agent-aarch64"''
                    else
                      ""
                  }

                # Create nix-bench symlink for nix run
                ln -s nix-bench-coordinator $out/bin/nix-bench
              '';
              meta.mainProgram = "nix-bench";
            };
          in
          {
            _module.args.pkgs = import nixpkgs {
              inherit system;
              config.allowUnfree = true;
              overlays = [ inputs.rust-overlay.overlays.default ];
            };

            # Tiered benchmark packages + Rust binaries
            packages =
              nixBench.mkTieredPackages { }
              // {
                "nix-bench" = nixBenchCombined;
                "nix-bench-coordinator" = nix-bench-coordinator;
                "nix-bench-agent" = nix-bench-agent;
                "nix-bench-nextest" = nix-bench-nextest;
                default = nixBenchCombined;
              }
              // (
                if nix-bench-agent-aarch64 != null then
                  { "nix-bench-agent-aarch64" = nix-bench-agent-aarch64; }
                else
                  { }
              );

            # Checks (run unit tests via nextest)
            checks = {
              nextest = nix-bench-nextest;
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
              programs.rustfmt.enable = true;
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

            # Development shell with cross-compilation support (musl for static binaries)
            devShells.default =
              let
                # Musl cross-compilation toolchains for static binaries
                aarch64MuslPkgs = pkgs.pkgsCross.aarch64-multiplatform-musl;
                x86_64MuslPkgs = pkgs.pkgsCross.musl64;
                aarch64MuslCc = aarch64MuslPkgs.stdenv.cc;
                x86_64MuslCc = x86_64MuslPkgs.stdenv.cc;
              in
              pkgs.mkShell {
                name = "nix-bench";
                nativeBuildInputs =
                  with pkgs;
                  [
                    nix-eval-jobs

                    # Rust development
                    rustToolchain
                    pkg-config
                    openssl
                    cargo-nextest
                    cargo-make

                    # AWS CLI for testing
                    awscli2
                  ]
                  ++ [ config.treefmt.build.wrapper ]
                  ++ (builtins.attrValues config.treefmt.build.programs)
                  ++ config.pre-commit.settings.enabledPackages;

                # Cross-compilation dependencies
                depsBuildBuild = [ pkgs.stdenv.cc ];

                shellHook = ''
                  ${config.pre-commit.installationScript}
                  echo "nix-bench dev shell"
                  echo ""
                  echo "  cargo make agent - Build agents (x86_64 + aarch64, static musl)"
                  echo "  cargo make b     - Build agents then coordinator"
                  echo "  cargo make r     - Build agents then run coordinator"
                  echo "  cargo make t     - Run tests with nextest"
                  echo "  cargo make ta    - Run all tests including AWS"
                  echo ""
                '';

                RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";

                # Cross-compilation environment for aarch64-musl (static linking)
                CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER = "${aarch64MuslCc}/bin/${aarch64MuslCc.targetPrefix}cc";
                CC_aarch64_unknown_linux_musl = "${aarch64MuslCc}/bin/${aarch64MuslCc.targetPrefix}cc";
                CXX_aarch64_unknown_linux_musl = "${aarch64MuslCc}/bin/${aarch64MuslCc.targetPrefix}c++";
                AR_aarch64_unknown_linux_musl = "${aarch64MuslCc}/bin/${aarch64MuslCc.targetPrefix}ar";

                # Cross-compilation environment for x86_64-musl (static linking)
                CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${x86_64MuslCc}/bin/${x86_64MuslCc.targetPrefix}cc";
                CC_x86_64_unknown_linux_musl = "${x86_64MuslCc}/bin/${x86_64MuslCc.targetPrefix}cc";
                CXX_x86_64_unknown_linux_musl = "${x86_64MuslCc}/bin/${x86_64MuslCc.targetPrefix}c++";
                AR_x86_64_unknown_linux_musl = "${x86_64MuslCc}/bin/${x86_64MuslCc.targetPrefix}ar";

                # Ensure native CC is used for build scripts
                HOST_CC = "${pkgs.stdenv.cc}/bin/cc";
              };
          };
      }
    );
}
