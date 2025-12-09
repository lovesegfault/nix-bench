{
  description = "nix-bench - benchmarking library for Nix build systems";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";

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
          in
          {
            _module.args.pkgs = import nixpkgs {
              inherit system;
              config.allowUnfree = true;
            };

            # Tiered benchmark packages
            packages = nixBench.mkTieredPackages { };

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
              nativeBuildInputs = [
                config.treefmt.build.wrapper
              ]
              ++ (builtins.attrValues config.treefmt.build.programs)
              ++ config.pre-commit.settings.enabledPackages;
              shellHook = ''
                ${config.pre-commit.installationScript}
              '';
            };
          };
      }
    );
}
