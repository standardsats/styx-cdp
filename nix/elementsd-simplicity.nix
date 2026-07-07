# Simplicity-capable elementsd. Simplicity is now in the mainline Elements release, so we
# build from a release tag instead of the old `simplicity` branch. The pre-2025 nodes on
# that branch reject the testnet's April blocks; this tag accepts them.
#
# This no longer matches SimplicityHL's cached store path, so the first build compiles
# Elements from source (slow, ~once per rev).
#
# To bump: update `rev`/`version`, set `sha256` to the fake below, build once, and paste
# the expected hash nix reports.
{ pkgs }:
pkgs.elementsd.overrideAttrs (_: {
  version = "liquid-testnet-23.3.4rc1";
  src = pkgs.fetchFromGitHub {
    owner = "ElementsProject";
    repo = "elements";
    rev = "7fd0885771cbe2ddba8a8e48cfa150390b5e5cbc"; # tag liquid-testnet-23.3.4rc1
    sha256 = "sha256-FAgSPWsZnqbo8XDaareMYIhrqjPTpkWzBweDwciiYtc=";
  };
  # The base derivation carries the mapport.cpp/miniupnpc patch; this tag already has it
  # upstream, so it no longer applies. Drop it.
  patches = [ ];
  withWallet = true;
  withGui = false;
  doCheck = false;
})
