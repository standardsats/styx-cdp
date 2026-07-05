# Simplicity-capable elementsd, built from the ElementsProject `simplicity` branch.
#
# Vendored verbatim from SimplicityHL (bitcoind-tests/elementsd-simplicity.nix). When
# this `rev` is evaluated under the nixpkgs pin in ../flake.nix (nixos-24.11, the same
# rev SimplicityHL locks), the derivation hashes to the store path that is already
# built, so `nix develop` reuses it instead of recompiling from source.
#
# To bump: update `rev` to the latest `simplicity` branch commit, set `sha256` to the
# placeholder below, rebuild once, and paste the expected hash nix reports.
{ pkgs }:
pkgs.elementsd.overrideAttrs (_: {
  version = "liquid-testnet-2024-10-08";
  src = pkgs.fetchFromGitHub {
    owner = "ElementsProject";
    repo = "elements";
    rev = "f957d3cde17c85afb18c6747f9c0b4fcb599f19a"; # `simplicity` branch
    sha256 = "sha256-XzdfbrQ7s4PfM5N00oP1jo5BNmD4WUMUe79QsTxsL4s=";
  };
  withWallet = true;
  withGui = false;
  doCheck = false;
})
