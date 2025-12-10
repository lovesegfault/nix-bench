# nix-bench

A Nix library for benchmarking Nix build systems by generating cache-busted package sets.

## Overview

nix-bench provides three cache-busting strategies for benchmarking:

- **Shallow**: Only the target package is rebuilt; dependencies use the cache
- **Deep**: The target AND all its dependencies are rebuilt
- **Mixed**: Combines shallow and deep variants in a single package

Fixed-output derivations (fetchurl, fetchFromGitHub, etc.) are content-addressed and always use the cache - only actual builds are affected.

## Usage

### Building Individual Packages

```bash
# Shallow: rebuild only the target package
nix build github:lovesegfault/nix-bench#shallow.hello --impure

# Deep: rebuild target + all dependencies
nix build github:lovesegfault/nix-bench#deep.firefox --impure

# Nested attributes work
nix build github:lovesegfault/nix-bench#shallow.chromium.browser --impure
```

### Tiered Benchmark Sets

The flake provides pre-configured benchmark packages:

```bash
# List available packages
nix flake show github:lovesegfault/nix-bench --impure

# Build a tier
nix build github:lovesegfault/nix-bench#small-shallow --impure
nix build github:lovesegfault/nix-bench#large-deep-4x --impure
```

Package naming: `{tier}-{kind}[-{multiplier}x]`

- **tier**: `small`, `medium`, `large`
- **kind**: `shallow`, `deep`, `mixed`
- **multiplier**: `2x`, `4x`, `8x`, `16x` (optional, increases package count)

### Default Tiers

| Tier   | Packages |
|--------|----------|
| small  | hello, coreutils, findutils, gnused, gnugrep |
| medium | small + git, curl, vim, python3, openssh, cmake |
| large  | medium + clang, gcc, firefox, rustc, chromium, kernel, libreoffice |

### Multipliers

Multipliers scale package count by duplicating packages with unique cache-busting values:

| Tier   | Base | 2x  | 4x  | 8x   | 16x  |
|--------|------|-----|-----|------|------|
| small  | 5    | 10  | 20  | 40   | 80   |
| medium | 11   | 22  | 44  | 88   | 176  |
| large  | 18   | 36  | 72  | 144  | 288  |

Each duplicated package has a unique impurity value, ensuring all instances rebuild independently.

### Strategy Selection

| Strategy | Use When | Build Cost |
|----------|----------|------------|
| **Shallow** | Measuring single-package build time | Low |
| **Deep** | Benchmarking full dependency rebuilds | High |
| **Mixed** | Strenuous stress testing | Extreme |

### Direct Package Access

Access any nixpkgs attribute directly with cache-busting via `legacyPackages`:

```bash
# Any package from nixpkgs with shallow cache-busting
nix build .#legacyPackages.x86_64-linux.shallow.python3Packages.numpy --impure

# Any package with deep cache-busting
nix build .#legacyPackages.x86_64-linux.deep.nodePackages.typescript --impure
```

## API Reference

### Quick Start

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nix-bench.url = "github:lovesegfault/nix-bench";
  };

  outputs = { nixpkgs, nix-bench, ... }:
    let
      pkgs = import nixpkgs { system = "x86_64-linux"; };
      bench = nix-bench.lib.mkNixBench { inherit pkgs; };
    in {
      packages.x86_64-linux = {
        # Individual packages with cache-busting
        hello-shallow = bench.shallowPkgs.hello;
        firefox-deep = bench.deepPkgs.firefox;
        chromium-browser = bench.shallowPkgs.chromium.browser;

        # Or use tiered benchmark sets
      } // bench.mkTieredPackages { };
    };
}
```


### `mkNixBench { pkgs, impurity? }`

Create a nix-bench instance with captured pkgs and impurity value.

Returns:
- `shallowPkgs` - pkgs mirror; any access returns a shallow cache-busted package
- `deepPkgs` - pkgs mirror; any access rebuilds the entire dependency tree
- `cacheBust` - apply shallow cache-busting to a single derivation
- `mkTieredPackages` - generate benchmark package sets
- `defaultTiers` - default tier definitions

### `mkTieredPackages { tiers?, multipliers?, kinds? }`

Generate tiered benchmark package sets.

Options:
- `tiers` - Attrset of tier definitions. Default: `defaultTiers`
- `multipliers` - List of multipliers. Default: `[ 1 2 4 8 16 ]`
- `kinds` - Which variants to generate. Default: `[ "shallow" "deep" "mixed" ]`

Example:
```nix
bench.mkTieredPackages {
  tiers = {
    minimal = [ "hello" "coreutils" ];
    web = [ "firefox" "chromium.browser" ];
  };
  multipliers = [ 1 2 4 ];
  kinds = [ "shallow" "deep" ];
}
```

### Custom Tiers

Tiers can be defined as lists of attribute paths:

```nix
tiers = {
  myTier = [ "hello" "python3Packages.requests" "chromium.browser" ];
}
```

Or as functions for more control:

```nix
tiers = {
  myTier = pkgs: with pkgs; [ hello python3 ];
}
```

## How It Works

### Shallow Cache-Busting

Injects `__IMPURE_NIXBENCH` environment variable into the derivation using `overrideAttrs`. This changes the derivation hash, forcing a rebuild of just that package.

### Deep Cache-Busting

Applies an overlay to `stdenv.mkDerivation` that injects the env var into ALL derivations. This forces rebuilding the entire dependency tree.

### Fixed-Output Derivations

Source fetchers (fetchurl, fetchFromGitHub, etc.) produce fixed-output derivations (FODs). These are content-addressed - their output hash is determined by the content, not the derivation inputs. This means FODs always use the cache, even with deep cache-busting.

## Requirements

- Nix with flakes enabled
- `--impure` flag required for cache-busting (uses `builtins.currentTime`)
- **Supported systems**: x86_64-linux, aarch64-linux

## Development

```bash
# Enter dev shell
nix develop --impure

# Format code
nix fmt

# Run checks
nix flake check --impure
```
