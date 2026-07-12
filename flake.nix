{
  description = "cdp-styx-v1: the STYX v1 CDP testnet implementation - dev shell";

  inputs = {
    # Recent nixpkgs for the Rust toolchain. The dependency tree needs rustc >= 1.88
    # (ar_archive_writer, home); nixos-25.11 ships 1.91. Also provides cc.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    # Base package set for the elementsd derivation we override in nix/elementsd-simplicity.nix.
    # We build Elements from a release tag now (Simplicity is upstream), so this no longer has
    # to match SimplicityHL's rev; it could fold into nixpkgs above, kept separate for now.
    nixpkgs-elements.url = "github:NixOS/nixpkgs/59e618d90c065f55ae48446f307e8c09565d5ab0";
  };

  outputs = { self, nixpkgs, nixpkgs-elements }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      elementsd-simplicity =
        (import nixpkgs-elements { inherit system; }).callPackage ./nix/elementsd-simplicity.nix { };

      # The workspace binaries, built hermetically from the flake's pinned inputs: the same
      # flake rev produces the same binaries, and therefore the same container layers.
      styx = pkgs.rustPlatform.buildRustPackage {
        pname = "styx";
        version = "0.1.0";
        # Only what the build reads: doc and deploy edits must not rebuild the world.
        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./crates
            ./covenants
            # The app embeds the spec-site faces (one source of truth); fonts only - doc
            # edits still must not rebuild the world.
            ./spec-site/fonts
          ];
        };
        cargoLock = {
          lockFile = ./Cargo.lock;
          outputHashes = {
            "simplicityhl-0.6.0-rc.0" = "sha256-mHWcALazH+kHfwRwaBKWgXzh458UOwJmReOgmvdh+Kg=";
          };
        };
        # The test tiers run in the dev shell / CI; the package build only produces the
        # deployment artifact.
        doCheck = false;
      };

      imageTag = self.shortRev or self.dirtyShortRev or "dev";

      # The health check as a self-contained app (shellcheck runs at build time), and a
      # loop around it for the monitor container - journald/timers belong to hosts,
      # `docker logs` to containers.
      check-health = pkgs.writeShellApplication {
        name = "check-health.sh";
        runtimeInputs = [ pkgs.curl pkgs.gnused pkgs.gnugrep pkgs.coreutils ];
        text = builtins.readFile ./deploy/check-health.sh;
      };
      monitor-loop = pkgs.writeShellApplication {
        name = "styx-monitor-loop";
        runtimeInputs = [ check-health pkgs.coreutils ];
        text = ''
          CONFIG="''${1:-/etc/styx/styxnet.toml}"
          INTERVAL="''${2:-60}"
          while true; do
            check-health.sh "$CONFIG" || true
            sleep "$INTERVAL"
          done
        '';
      };

      # One role, one image. Minimal closures (no shell); certificates included for the
      # exchange feeds and wss relays; deterministic creation timestamp is dockerTools'
      # default, so an image is repeatable from the flake rev alone.
      #
      # Daemons run as an unprivileged numeric uid (no /etc/passwd needed for static-ish
      # Rust binaries). A role with on-disk state names its stateDir, created in the image
      # with matching ownership so a named volume initializes writable. elementsd is the
      # exception and stays root: /data is host-managed, its ownership is the deployment's
      # call (chown the volume and add `user:` in compose to drop it).
      nonRootUid = "65532";
      mkImage = { name, entrypoint, contents, stateDir ? null }:
        pkgs.dockerTools.buildLayeredImage {
          inherit name;
          tag = imageTag;
          contents = contents ++ [ pkgs.cacert ];
          fakeRootCommands = pkgs.lib.optionalString (stateDir != null) ''
            mkdir -p .${stateDir}
            chown ${nonRootUid}:${nonRootUid} .${stateDir}
          '';
          config = {
            User = nonRootUid;
            Entrypoint = entrypoint;
            Env = [ "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt" ];
          };
        };
    in
    {
      packages.${system} = {
        elementsd = elementsd-simplicity;
        inherit styx;
        oracle-image = mkImage {
          name = "styx-oracle";
          entrypoint = [ "${styx}/bin/styx-oracle" ];
          contents = [ styx ];
        };
        keeper-image = mkImage {
          name = "styx-keeper";
          entrypoint = [ "${styx}/bin/styx-keeper" ];
          contents = [ styx ];
          stateDir = "/var/lib/styx"; # the indexer snapshot
        };
        # No entrypoint: `docker run styx-tools /bin/styx-wallet ...` or /bin/styx-deploy.
        tools-image = mkImage {
          name = "styx-tools";
          entrypoint = [ ];
          contents = [ styx ];
          stateDir = "/var/lib/styx";
        };
        elementsd-image = pkgs.dockerTools.buildLayeredImage {
          name = "styx-elementsd";
          tag = imageTag;
          contents = [ elementsd-simplicity ];
          extraCommands = "mkdir -p data tmp";
          config = {
            Entrypoint = [ "${elementsd-simplicity}/bin/elementsd" "-datadir=/data" ];
            Volumes = { "/data" = { }; };
          };
        };
        explorer-image = mkImage {
          name = "styx-explorer";
          entrypoint = [ "${styx}/bin/styx-explorer" ];
          contents = [ styx ];
          stateDir = "/var/lib/styx"; # the indexer snapshot
        };
        relay-image = mkImage {
          name = "styx-relay";
          entrypoint = [ "${pkgs.nostr-rs-relay}/bin/nostr-rs-relay" ];
          contents = [ pkgs.nostr-rs-relay ];
          stateDir = "/var/lib/relay"; # the event database
        };
        monitor-image = mkImage {
          name = "styx-monitor";
          entrypoint = [ "${monitor-loop}/bin/styx-monitor-loop" ];
          contents = [ monitor-loop ];
        };
      };

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
          pkgs.openssl # deploy/join-testnet.sh generates the node's rpcauth line
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
