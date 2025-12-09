/**
  nix-bench: Nix build benchmarking library.

  Generate cache-busted package sets for benchmarking Nix build systems.
  Supports shallow (top-level only) and deep (full dependency tree) cache-busting.

  Fixed-output derivations (fetchurl, fetchFromGitHub, etc.) are content-addressed
  and always use the cache - only actual builds are affected.
*/
rec {
  /**
    Default benchmark tiers as lists of nixpkgs attribute paths.

    Tiers are cumulative: medium includes small, large includes medium.

    - `small`: 5 fast-building CLI tools
    - `medium`: 11 packages including git, python, cmake
    - `large`: 18 packages including browsers, compilers, kernel
  */
  defaultTiers = {
    small = [
      "hello"
      "coreutils"
      "findutils"
      "gnused"
      "gnugrep"
    ];
    medium = defaultTiers.small ++ [
      "git"
      "curl"
      "vim"
      "python3"
      "openssh"
      "cmake"
    ];
    large = defaultTiers.medium ++ [
      "llvmPackages.clang-unwrapped"
      "gcc-unwrapped"
      "firefox.unwrapped"
      "rustc.unwrapped"
      "chromium.browser"
      "linuxPackages.kernel"
      "libreoffice.unwrapped"
    ];
  };

  /**
    Shallow cache-bust a single package.

    Injects `__IMPURE_NIXBENCH` env var into the derivation, changing
    its hash and forcing a rebuild. Dependencies are NOT affected.

    # Inputs

    `impurity`

    : Value that changes between builds (e.g., `builtins.currentTime`)

    `pkg`

    : Derivation to cache-bust

    # Type

    ```
    mkUncached :: Int -> Derivation -> Derivation
    ```

    # Examples
    :::{.example}
    ## `lib.mkUncached` usage example

    ```nix
    mkUncached builtins.currentTime pkgs.hello
    => «derivation /nix/store/...-hello-2.12.2.drv»
    ```

    :::
  */
  mkUncached =
    impurity: pkg:
    pkg.overrideAttrs (old: {
      env = (old.env or { }) // {
        __IMPURE_NIXBENCH = toString impurity;
      };
    });

  /**
    Create a pkgs instance with deep cache-busting.

    Overlays `stdenv.mkDerivation` to inject the env var into ALL derivations,
    forcing rebuilds of the entire dependency tree.

    Fixed-output derivations (fetchers) are content-addressed and still use
    the cache - only actual builds are affected.

    # Inputs

    `impurity`

    : Value that changes between builds (e.g., `builtins.currentTime`)

    `pkgs`

    : nixpkgs instance to overlay

    # Type

    ```
    mkPkgsDeep :: Int -> Pkgs -> Pkgs
    ```

    # Examples
    :::{.example}
    ## `lib.mkPkgsDeep` usage example

    ```nix
    deepPkgs = mkPkgsDeep builtins.currentTime pkgs;
    deepPkgs.hello
    => «derivation ...»  # hello AND all deps will rebuild
    ```

    :::
  */
  mkPkgsDeep =
    impurity: pkgs:
    pkgs.extend (
      _: prev: {
        stdenv = prev.stdenv // {
          mkDerivation =
            args:
            let
              injectEnv =
                attrs:
                attrs
                // {
                  env = (attrs.env or { }) // {
                    __IMPURE_NIXBENCH = toString impurity;
                  };
                };
              newArgs =
                if builtins.isFunction args then finalAttrs: injectEnv (args finalAttrs) else injectEnv args;
            in
            prev.stdenv.mkDerivation newArgs;
        };
      }
    );

  /**
    Create a nix-bench instance with captured impurity and pkgs.

    This is the main entry point. Returns an attrset with `shallowPkgs`,
    `deepPkgs`, and `mkTieredPackages` for generating benchmark sets.

    # Inputs

    `pkgs`

    : nixpkgs instance (required)

    `impurity`

    : Cache-busting seed. Default: `builtins.currentTime`

    # Type

    ```
    mkNixBench :: { pkgs : Pkgs, impurity ? Int } -> {
      shallowPkgs : Pkgs,
      deepPkgs : Pkgs,
      cacheBust : Derivation -> Derivation,
      mkTieredPackages : { ... } -> AttrSet,
      defaultTiers : AttrSet,
    }
    ```

    # Examples
    :::{.example}
    ## `lib.mkNixBench` usage example

    ```nix
    nixBench = mkNixBench { inherit pkgs; };

    # Shallow cache-bust (deps stay cached)
    nixBench.shallowPkgs.hello
    nixBench.shallowPkgs.chromium.browser  # nested attrs work

    # Deep cache-bust (rebuild entire dep tree)
    nixBench.deepPkgs.firefox

    # Generate tiered benchmark sets
    nixBench.mkTieredPackages { }
    nixBench.mkTieredPackages {
      tiers = { minimal = [ "hello" "coreutils" ]; };
      multipliers = [ 1 2 4 ];
    }
    ```

    :::
  */
  mkNixBench =
    {
      pkgs,
      impurity ? defaultImpurity,
    }:
    let
      inherit (pkgs) lib;
      cacheBust = mkUncached impurity;
      deepPkgs = mkPkgsDeep impurity pkgs;

      # Lazy wrapper that cache-busts packages on access.
      # Handles nested attrs like chromium.browser correctly.
      shallowPkgs =
        let
          wrap =
            value:
            if lib.isDerivation value then
              let
                # Only overlay sub-packages, not internal derivation attrs
                subPkgs = lib.filterAttrs (
                  _: v: lib.isDerivation v || (builtins.isAttrs v && !(v ? drvPath))
                ) value;
              in
              cacheBust value // lib.mapAttrs (_: wrap) subPkgs
            else if builtins.isAttrs value then
              lib.mapAttrs (_: wrap) value
            else
              value;
        in
        wrap pkgs;

      # Resolve "foo.bar.baz" -> pkgs.foo.bar.baz
      resolvePkg = path: lib.getAttrFromPath (lib.splitString "." path) pkgs;

      # Normalize tier: accept list of attr paths or function
      normalizeTier = tier: if builtins.isFunction tier then tier else _: map resolvePkg tier;

      # Generate {shallow,deep,mixed} variants for one tier at one multiplier
      mkTierVariants =
        tierName: tierFn: mult:
        let
          uuids = lib.genList (i: impurity + i) mult;
          suffix = lib.optionalString (mult > 1) "-${toString mult}x";

          shallowPkg = pkgs.symlinkJoin {
            name = "nix-bench-${tierName}-shallow${suffix}";
            paths = lib.concatMap (uuid: map (mkUncached uuid) (tierFn pkgs)) uuids;
          };

          deepPkg = pkgs.symlinkJoin {
            name = "nix-bench-${tierName}-deep${suffix}";
            paths = lib.concatMap (uuid: tierFn (mkPkgsDeep uuid pkgs)) uuids;
          };

          mixedPkg = pkgs.symlinkJoin {
            name = "nix-bench-${tierName}-mixed${suffix}";
            paths = [
              shallowPkg
              deepPkg
            ];
          };
        in
        {
          "${tierName}-shallow${suffix}" = shallowPkg;
          "${tierName}-deep${suffix}" = deepPkg;
          "${tierName}-mixed${suffix}" = mixedPkg;
        };

      /**
        Generate tiered benchmark package sets.

        # Inputs

        `tiers`

        : Attrset of tier name -> list of attr paths or function.
          Default: `defaultTiers`

        `multipliers`

        : List of package multipliers (1 = normal, 2 = 2x packages).
          Default: `[ 1 2 4 8 16 ]`

        `kinds`

        : Which variants to generate.
          Default: `[ "shallow" "deep" "mixed" ]`

        # Type

        ```
        mkTieredPackages :: { ... } -> AttrSet
        ```
      */
      mkTieredPackages =
        {
          tiers ? defaultTiers,
          multipliers ? [
            1
            2
            4
            8
            16
          ],
          kinds ? [
            "shallow"
            "deep"
            "mixed"
          ],
        }:
        let
          normalized = lib.mapAttrs (_: normalizeTier) tiers;
          all = lib.concatMapAttrs (
            name: fn: lib.mergeAttrsList (map (mkTierVariants name fn) multipliers)
          ) normalized;
          pat = "(${lib.concatStringsSep "|" kinds})";
        in
        lib.filterAttrs (n: _: builtins.match ".*-${pat}(-[0-9]+x)?$" n != null) all;

    in
    {
      inherit
        shallowPkgs
        deepPkgs
        cacheBust
        mkTieredPackages
        defaultTiers
        ;
    };

  /**
    Default impurity value using `builtins.currentTime`.

    Falls back to 0 with a warning in pure evaluation mode.
    Use `--impure` flag with nix commands for cache-busting to work.

    # Type

    ```
    defaultImpurity :: Int
    ```
  */
  defaultImpurity =
    builtins.currentTime
      or (builtins.warn "nix-bench: pure eval mode, pass --impure for cache-busting" 0);
}
