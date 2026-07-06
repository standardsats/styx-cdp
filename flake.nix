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
          pkgs.rustfmt
          pkgs.cargo-llvm-cov
          pkgs.gcc # the cc-wrapper rustc needs to link
          elementsd-simplicity
          # The oracle quote relay (R5): one instance on the infra machine; the swarm
          # rehearsal launches it locally.
          pkgs.nostr-rs-relay
          pkgs.curl # the scenario scripts drive the oracle admin endpoints
        ];

        # The e2e tier (styx-node tests) reads this to find the Simplicity-capable node.
        ELEMENTSD_EXE = "${elementsd-simplicity}/bin/elementsd";

        # cargo-llvm-cov normally fetches the rustup llvm-tools component; point it at the
        # llvm-cov/llvm-profdata from the same LLVM this rustc was built with instead.
        LLVM_COV = "${pkgs.rustc.llvmPackages.llvm}/bin/llvm-cov";
        LLVM_PROFDATA = "${pkgs.rustc.llvmPackages.llvm}/bin/llvm-profdata";

        shellHook = ''
          echo "styx v1 dev shell"
          echo "  rustc     $(rustc --version | cut -d' ' -f2)"
          echo "  elementsd $ELEMENTSD_EXE"
          echo ""
          echo "  fast tiers : cargo test --workspace          # unit + prune-level, no node"
          echo "  e2e        : cargo test -p styx-node -- --ignored"
          echo "  format     : cargo fmt --all [--check]"
          echo "  coverage   : cargo llvm-cov --workspace [--html]"
        '';
      };
    };
}
