# The STYX v1 testnet stack as a NixOS module: one liquidtestnet node shared by five
# oracles, the quote relay, the read-only explorer, a keeper, a health monitor, and the
# static landing + spec site. The protocol itself is five frozen covenants with no admin
# key; only these processes are operated.
#
# The module runs every role as a real process on one host, sharing ONE node (the swarm
# shape). The federation makes the blocks, so there is no producer and no self-mine.
#
# Two publication modes, chosen by `tls`:
#   tls = true   nginx fronts the public hosts with ACME (a plain clearnet deployment).
#   tls = false  nginx listens on loopback ports only; something else (a Tor onion service,
#                an upstream proxy) exposes them. This is the default.
#
# The non-secret half of every config is baked here; the secret half is merged at preStart
# from `secretsDir` (out-of-store files: rpcauth, oracle keys, explorer/keeper passwords).
{ config, lib, pkgs, ... }:

let
  cfg = config.services.styx-testnet;
  inherit (lib) mkIf mkOption mkEnableOption types;

  styx = cfg.package;
  elementsd = cfg.elementsd;
  nostrRelay = cfg.nostrRelay;

  rpcPort = cfg.ports.rpc;
  relayPort = cfg.ports.relay;
  explorerPort = cfg.ports.explorer;
  oraclePort = slot: cfg.ports.oracleBase + slot;

  secretsDir = cfg.secretsDir;
  stateDir = cfg.stateDir;
  publishedConfig = "${stateDir}/liquid-testnet.toml";

  # Static web: landing at /, spec-site under /spec/. Built from the flake's own source, so
  # the site ships the same rev as the binaries.
  webroot = pkgs.runCommand "styx-web" { } ''
    mkdir -p $out
    cp -r ${../landing}/. $out/
    mkdir -p $out/spec
    cp -r ${../spec-site}/. $out/spec/
  '';

  # What nginx serves while the explorer daemon restarts: a themed 503 that retries itself,
  # instead of the bare 502. No JS, like the explorer.
  explorerHolding = pkgs.writeTextDir "styx-restarting.html" ''
    <!doctype html>
    <html lang="en">
    <head>
    <meta charset="utf-8">
    <meta http-equiv="refresh" content="3">
    <title>STYX explorer</title>
    <style>
      body { background: #0b0e12; color: #b9b3a3; font: 16px/1.6 Georgia, serif;
             min-height: 100vh; margin: 0; display: grid; place-items: center; }
      main { text-align: center; }
      h1 { color: #eae5d6; font-weight: 400; letter-spacing: 0.06em; }
      p { color: #7e7b6e; }
    </style>
    </head>
    <body>
    <main>
    <h1>The explorer is restarting</h1>
    <p>Back in a few seconds. This page retries on its own.</p>
    </main>
    </body>
    </html>
  '';

  checkHealth = pkgs.writeShellApplication {
    name = "styx-check-health";
    runtimeInputs = [ pkgs.curl pkgs.gnused pkgs.gnugrep pkgs.coreutils ];
    text = builtins.readFile ../deploy/check-health.sh;
  };

  # --- config fragments (the non-secret half) ---

  # elementsd: a liquidtestnet follower. The rpcauth line is secret, appended at start.
  nodeConfBase = pkgs.writeText "elements.conf" ''
    chain=liquidtestnet
    validatepegin=0
    fallbackfee=0.0001
    # Protocol outputs are explicit; the node wallet is only the tL-BTC on-ramp.
    blindedaddresses=0
    txindex=1
    # STYX issues OBOL past the old 21M-unit cap. The consensus rule is active on
    # liquidtestnet, but createrawtransaction's amount validator still rejects >21M
    # unblinded issuance without this, and the deploy ceremony fails with RPC -3.
    acceptunlimitedissuances=1

    [liquidtestnet]
    rpcport=${toString rpcPort}
    rpcbind=127.0.0.1
    rpcallowip=127.0.0.1
  '';

  relayConf = pkgs.writeText "nostr-relay.toml" ''
    [info]
    relay_url = "wss://${cfg.relayHost}"
    name = "styxnet testnet relay"

    [database]
    data_directory = "/var/lib/nostr-rs-relay"

    [network]
    address = "127.0.0.1"
    port = ${toString relayPort}
  '';

  # Per-slot oracle fragment. The three secrets (protocol_seckey, nostr_seckey,
  # rpc_password) come from secretsDir/oracle-<slot>.secret.toml, prepended at start so
  # every bare key precedes the [feed] table.
  oraclePublic = slot: backend: pkgs.writeText "oracle-${toString slot}.public.toml" ''
    slot = ${toString slot}
    # Self-declared label the explorer shows instead of the bare slot: the price source.
    name = "${backend}"
    relays = ["ws://127.0.0.1:${toString relayPort}"]
    rpc_url = "http://127.0.0.1:${toString rpcPort}"
    rpc_user = "styx"
    listen = "127.0.0.1:${toString (oraclePort slot)}"
    price_usd = 120000
    poll_ms = 500

    [feed]
    backend = "${backend}"
    poll_ms = 5000
    # Staleness gate: stop publishing when this exchange goes quiet, so five gated oracles
    # freeze the quorum safely instead of re-signing a stale price.
    max_age_secs = 60
  '';

  explorerPublic = pkgs.writeText "explorer.public.toml" ''
    styxnet = "${publishedConfig}"
    rpc_url = "http://127.0.0.1:${toString rpcPort}"
    rpc_user = "styx"
    snapshot = "${stateDir}/explorer-snapshot.json"
    listen = "127.0.0.1:${toString explorerPort}"
    poll_ms = 2000
    app_url = "${cfg.appUrl}"
  '';

  # systemd hardening common to the long-running Rust daemons.
  daemonHardening = {
    NoNewPrivileges = true;
    PrivateTmp = true;
    ProtectSystem = "strict";
    ProtectHome = true;
    ProtectKernelTunables = true;
    ProtectKernelModules = true;
    ProtectControlGroups = true;
    RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
    RestrictNamespaces = true;
    LockPersonality = true;
    RestrictRealtime = true;
  };

  # One systemd unit per oracle slot.
  oracleServices = lib.listToAttrs (lib.imap0
    (slot: backend: lib.nameValuePair "styx-oracle-${toString slot}" {
      description = "STYX oracle daemon (slot ${toString slot}, ${backend})";
      after = [ "styx-elementsd.service" ];
      requires = [ "styx-elementsd.service" ];
      wantedBy = [ "multi-user.target" ];
      preStart = ''
        umask 077
        cat ${secretsDir}/oracle-${toString slot}.secret.toml ${oraclePublic slot backend} \
          > "$RUNTIME_DIRECTORY/oracle.toml"
      '';
      serviceConfig = daemonHardening // {
        User = "styx";
        Group = "styx";
        RuntimeDirectory = "styx-oracle-${toString slot}";
        RuntimeDirectoryMode = "0700";
        ExecStart = "${styx}/bin/styx-oracle --config /run/styx-oracle-${toString slot}/oracle.toml";
        Restart = "on-failure";
        RestartSec = 5;
      };
    })
    cfg.oracleBackends);

  # nginx vhost bodies, shared by both publication modes.
  siteLocations = {
    "/".tryFiles = "$uri $uri/ /index.html";
    "/spec/".tryFiles = "$uri $uri/ =404";
  };

  # Explorer, plus the /config download the wallet quickstart points at. The page refreshes
  # itself, so a daemon restart is seen by open tabs: show the holding page (as 503).
  explorerVhostBody = {
    extraConfig = ''
      error_page 502 503 504 =503 /styx-restarting.html;
    '';
    locations."= /styx-restarting.html" = {
      root = explorerHolding;
      extraConfig = "internal;";
    };
    locations."= /config" = {
      alias = publishedConfig;
      extraConfig = ''
        default_type text/plain;
        add_header Content-Disposition "inline; filename=liquid-testnet.toml";
      '';
    };
    locations."/" = {
      proxyPass = "http://127.0.0.1:${toString explorerPort}";
      proxyWebsockets = true;
    };
  };

  relayVhostBody = {
    locations."/" = {
      proxyPass = "http://127.0.0.1:${toString relayPort}";
      proxyWebsockets = true;
    };
  };

  tlsBase = { enableACME = true; forceSSL = true; };
  loopback = port: [{ addr = "127.0.0.1"; inherit port; } { addr = "[::1]"; inherit port; }];

  # Clearnet: one ACME vhost per host. Onion mode: loopback listeners keyed by port, reached
  # by whatever fronts them (the relay is reached directly at its own port, no vhost).
  clearnetVhosts =
    (lib.genAttrs cfg.webHosts (_: tlsBase // { root = webroot; locations = siteLocations; }))
    // {
      "${cfg.explorerHost}" = tlsBase // explorerVhostBody;
      "${cfg.relayHost}" = tlsBase // relayVhostBody;
    };
  onionVhosts = {
    "styx-site" = { listen = loopback cfg.ports.siteHttp; root = webroot; locations = siteLocations; };
    "styx-explorer" = { listen = loopback cfg.ports.explorerHttp; } // explorerVhostBody;
  };
in
{
  imports = [ ./styx-monitoring.nix ./styx-mainnet.nix ];

  options.services.styx-testnet = {
    enable = mkEnableOption "the STYX v1 testnet stack (node, oracles, relay, explorer, keeper, site)";

    package = mkOption {
      type = types.package;
      default = pkgs.styx;
      defaultText = lib.literalExpression "pkgs.styx";
      description = "The styx workspace binaries (needs the flake's overlay applied).";
    };
    elementsd = mkOption {
      type = types.package;
      default = pkgs.elementsd-simplicity;
      defaultText = lib.literalExpression "pkgs.elementsd-simplicity";
      description = "The Simplicity-capable Elements node.";
    };
    nostrRelay = mkOption {
      type = types.package;
      default = pkgs.nostr-rs-relay;
      defaultText = lib.literalExpression "pkgs.nostr-rs-relay";
      description = "The nostr relay that carries the oracle quotes.";
    };

    oracleBackends = mkOption {
      type = types.listOf types.str;
      default = [ "coinbase" "binance" "kraken" "okx" "bitfinex" ];
      description = "One independent exchange per slot; list index is the slot number.";
    };

    secretsDir = mkOption {
      type = types.str;
      default = "/var/secrets/styx";
      description = ''
        Directory holding the out-of-store secret fragments, merged at preStart:
        node-rpcauth, oracle-<slot>.secret.toml, explorer.secret.toml, keeper.toml.
      '';
    };
    stateDir = mkOption {
      type = types.str;
      default = "/var/lib/styx";
      description = "World-readable state: the published config and the explorer/keeper snapshots.";
    };

    tls = mkOption {
      type = types.bool;
      default = false;
      description = "true: nginx fronts the public hosts with ACME. false: loopback only.";
    };

    webHosts = mkOption {
      type = types.listOf types.str;
      default = [ "styx.network" ];
      description = "Hosts the landing + spec site answer on (clearnet mode). First is canonical.";
    };
    explorerHost = mkOption {
      type = types.str;
      default = "explorer.testnet.styx.network";
      description = "Explorer host (clearnet mode).";
    };
    relayHost = mkOption {
      type = types.str;
      default = "relay.testnet.styx.network";
      description = "Relay host; also the relay_url the relay advertises over NIP-11.";
    };
    appUrl = mkOption {
      type = types.str;
      default = "https://styx.network";
      description = "The download/landing URL the explorer links to.";
    };

    ports = {
      rpc = mkOption { type = types.port; default = 18884; description = "elementsd RPC (loopback)."; };
      relay = mkOption { type = types.port; default = 7877; description = "nostr-rs-relay (loopback)."; };
      explorer = mkOption { type = types.port; default = 9790; description = "styx-explorer HTTP (loopback)."; };
      oracleBase = mkOption { type = types.port; default = 9700; description = "First oracle admin port; slot N binds base+N."; };
      siteHttp = mkOption { type = types.port; default = 8088; description = "nginx loopback listener for the site (onion mode)."; };
      explorerHttp = mkOption { type = types.port; default = 8089; description = "nginx loopback listener for the explorer (onion mode)."; };
    };
  };

  config = mkIf cfg.enable {
    # styx-wallet / styx-deploy / elements-cli on PATH for the ceremony.
    environment.systemPackages = [ styx elementsd nostrRelay ];

    users.users.styx = {
      isSystemUser = true;
      group = "styx";
      home = stateDir;
      description = "STYX testnet services";
    };
    users.groups.styx = { };

    # The secret dir is operator-filled (0750). The state dir must exist up front (0755):
    # the ceremony drops the published config there before the explorer ever starts.
    systemd.tmpfiles.rules = [
      "d ${secretsDir} 0750 root styx - -"
      "d ${stateDir} 0755 styx styx - -"
    ];

    systemd.services = {
      # The shared chain node.
      styx-elementsd = {
        description = "Elements node (Liquid testnet, Simplicity-capable)";
        after = [ "network-online.target" ];
        wants = [ "network-online.target" ];
        wantedBy = [ "multi-user.target" ];
        preStart = ''
          umask 077
          cat ${nodeConfBase} ${secretsDir}/node-rpcauth > "$STATE_DIRECTORY/elements.conf"
        '';
        serviceConfig = {
          User = "styx";
          Group = "styx";
          StateDirectory = "styxnet";
          StateDirectoryMode = "0750";
          ExecStart = "${elementsd}/bin/elementsd -datadir=/var/lib/styxnet";
          Restart = "on-failure";
          RestartSec = 5;
        };
      };

      styx-relay = {
        description = "nostr-rs-relay (STYX testnet quotes)";
        after = [ "network-online.target" ];
        wants = [ "network-online.target" ];
        wantedBy = [ "multi-user.target" ];
        serviceConfig = daemonHardening // {
          User = "styx";
          Group = "styx";
          StateDirectory = "nostr-rs-relay";
          ExecStart = "${nostrRelay}/bin/nostr-rs-relay --config ${relayConf}";
          Restart = "on-failure";
          RestartSec = 5;
        };
      };

      # Read-only explorer. Stays dormant (skipped, not failed) until the ceremony publishes
      # the config, so a pre-ceremony deploy activates cleanly instead of restart-looping.
      styx-explorer = {
        description = "STYX explorer (read-only view)";
        after = [ "styx-elementsd.service" "styx-relay.service" ];
        requires = [ "styx-elementsd.service" ];
        wantedBy = [ "multi-user.target" ];
        unitConfig.ConditionPathExists = publishedConfig;
        preStart = ''
          umask 077
          cat ${secretsDir}/explorer.secret.toml ${explorerPublic} \
            > "$RUNTIME_DIRECTORY/explorer.toml"
        '';
        serviceConfig = daemonHardening // {
          User = "styx";
          Group = "styx";
          RuntimeDirectory = "styx-explorer";
          RuntimeDirectoryMode = "0700";
          StateDirectory = "styx";
          ExecStart = "${styx}/bin/styx-explorer --config /run/styx-explorer/explorer.toml";
          Restart = "on-failure";
          RestartSec = 10;
        };
      };

      # The keeper liquidates any unhealthy vault. exit 65 = protocol bug, do not retry.
      # Dormant until both the published config and its own key config exist.
      styx-keeper = {
        description = "STYX keeper daemon";
        after = [ "styx-elementsd.service" ];
        requires = [ "styx-elementsd.service" ];
        wantedBy = [ "multi-user.target" ];
        unitConfig.ConditionPathExists = [ publishedConfig "${secretsDir}/keeper.toml" ];
        serviceConfig = daemonHardening // {
          User = "styx";
          Group = "styx";
          StateDirectory = "styx";
          ExecStart = "${styx}/bin/styx-keeper --config ${secretsDir}/keeper.toml";
          Restart = "on-failure";
          RestartSec = 5;
          RestartPreventExitStatus = 65;
        };
      };

      # One health pass; the timer below runs it every minute.
      styx-monitor = {
        description = "STYX deployment health check";
        unitConfig.ConditionPathExists = publishedConfig;
        serviceConfig = {
          Type = "oneshot";
          User = "styx";
          Group = "styx";
          ExecStart = "${checkHealth}/bin/styx-check-health ${publishedConfig}";
        };
      };
    } // oracleServices;

    systemd.timers.styx-monitor = {
      description = "STYX health check every minute";
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "2min";
        OnUnitActiveSec = "1min";
      };
    };

    services.nginx = {
      enable = true;
      recommendedProxySettings = true;
      recommendedOptimisation = true;
      virtualHosts = if cfg.tls then clearnetVhosts else onionVhosts;
    };
  };
}
