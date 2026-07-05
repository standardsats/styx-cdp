{
  description = "cdp-styx-v1: the STYX v1 CDP testnet implementation - dev shell";

  inputs = {
    # Recent nixpkgs for the Rust toolchain. The dependency tree needs rustc >= 1.88
    # (ar_archive_writer, home); nixos-25.11 ships 1.91. Also provides cc.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    # Pinned to the exact rev SimplicityHL builds its Simplicity elementsd from, so
    # the derivation reuses the already-built store path instead of recompiling.
    nixpkgs-elements.url = "github:NixOS/nixpkgs/59e618d90c065f55ae48446f307e8c09565d5ab0";
  };

  outputs = { self, nixpkgs, nixpkgs-elements }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      elementsd-simplicity =
        (import nixpkgs-elements { inherit system; }).callPackage ./nix/elementsd-simplicity.nix { };
    in
    {
      packages.${system}.elementsd = elementsd-simplicity;

      devShells.${system}.default = pkgs.mkShell {
        packages = [
          pkgs.rustc
          pkgs.cargo
          pkgs.clippy
          pkgs.gcc # the cc-wrapper rustc needs to link
          elementsd-simplicity
        ];

        # The e2e tier (styx-node tests) reads this to find the Simplicity-capable node.
        ELEMENTSD_EXE = "${elementsd-simplicity}/bin/elementsd";

        shellHook = ''
          echo "styx v1 dev shell"
          echo "  rustc     $(rustc --version | cut -d' ' -f2)"
          echo "  elementsd $ELEMENTSD_EXE"
          echo ""
          echo "  fast tiers : cargo test --workspace          # unit + prune-level, no node"
          echo "  e2e        : cargo test -p styx-node -- --ignored"
        '';
      };
    };
}
