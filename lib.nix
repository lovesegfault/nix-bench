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
    let
      inherit (pkgs) lib;

      injectEnv =
        attrs:
        attrs
        // {
          env = (attrs.env or { }) // {
            __IMPURE_NIXBENCH = toString impurity;
          };
        };

      # We override `mkDerivationFromStdenv` rather than just patching `stdenv.mkDerivation`
      # because this parameter is preserved through stdenv.override(). This means stdenv
      # variants like gccStdenv, clangStdenv, and llvmPackages.stdenv automatically inherit
      # our cache-busting without needing to patch each one individually.
      origMkDerivationFromStdenv =
        stdenv:
        (import "${pkgs.path}/pkgs/stdenv/generic/make-derivation.nix" {
          inherit lib;
          config = pkgs.config or { };
        } stdenv).mkDerivation;

      patchedMkDerivationFromStdenv =
        stdenv:
        let
          origMkDeriv = origMkDerivationFromStdenv stdenv;
        in
        args:
        let
          newArgs =
            if builtins.isFunction args then finalAttrs: injectEnv (args finalAttrs) else injectEnv args;
        in
        origMkDeriv newArgs;
    in
    pkgs.extend (
      _final: prev: {
        stdenv = prev.stdenv.override { mkDerivationFromStdenv = patchedMkDerivationFromStdenv; };
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
      #
      # The merge (`//`) below replaces sub-package attrs with wrapped versions.
      # Without this, `shallowPkgs.chromium.browser` would return the original
      # uncached browser since cacheBust only modifies the top-level derivation.
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

      # Normalize tier: accept list of attr paths or function
      # Returns a function: pkgs -> [derivations]
      normalizeTier =
        tier:
        if builtins.isFunction tier then
          tier
        else
          p: map (path: lib.getAttrFromPath (lib.splitString "." path) p) tier;

      # Generate {shallow,deep,mixed} variants for one tier at one multiplier
      mkTierVariants =
        tierName: tierFn: mult:
        let
          # Use hash to avoid collision when builds run within `mult` seconds of each other
          uuids = lib.genList (i: builtins.hashString "sha256" "${toString impurity}-${toString i}") mult;
          suffix = lib.optionalString (mult > 1) "-${toString mult}x";

          shallowPaths = lib.concatMap (uuid: map (mkUncached uuid) (tierFn pkgs)) uuids;
          deepPaths = lib.concatMap (uuid: tierFn (mkPkgsDeep uuid pkgs)) uuids;

          shallowPkg = pkgs.symlinkJoin {
            name = "nix-bench-${tierName}-shallow${suffix}";
            paths = shallowPaths;
          };

          deepPkg = pkgs.symlinkJoin {
            name = "nix-bench-${tierName}-deep${suffix}";
            paths = deepPaths;
          };

          # Use flat dependencies directly instead of intermediate meta-packages
          # This avoids issues with build monitoring tools that stop at "irrelevant" intermediate nodes
          mixedPkg = pkgs.symlinkJoin {
            name = "nix-bench-${tierName}-mixed${suffix}";
            paths = shallowPaths ++ deepPaths;
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

    Throws in pure evaluation mode to prevent silently broken benchmarks.
    Use `--impure` flag with nix commands for cache-busting to work.

    # Type

    ```
    defaultImpurity :: Int
    ```
  */
  defaultImpurity =
    builtins.currentTime
      or (throw "nix-bench: requires --impure flag (builtins.currentTime unavailable in pure eval mode)");
}
