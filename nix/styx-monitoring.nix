# Prometheus + Grafana for the STYX testnet stack: the node, the relay, the five oracles,
# the keeper, and the protocol singletons (pot/reserve/issuer). Everything binds loopback;
# reach Grafana over an SSH tunnel or an onion service.
#
# A 30s timer runs one collector that scrapes the node RPC, the relay, the oracle /health
# surfaces, the explorer /api/state, and the keeper wallet into a node-exporter textfile;
# Prometheus scrapes node-exporter; Grafana provisions the dashboard.
{ config, lib, pkgs, ... }:

let
  cfg = config.services.styx-testnet;
  mcfg = cfg.monitoring;

  styx = cfg.package;
  elementsd = cfg.elementsd;

  relayPort = cfg.ports.relay;
  explorerPort = cfg.ports.explorer;
  oracleBase = cfg.ports.oracleBase;

  grafanaPort = mcfg.grafanaPort;
  promPort = 9090;
  nodeExporterPort = 9100;

  textfileDir = "/var/lib/prometheus-node-exporter-text";
  secretsDir = cfg.secretsDir;
  keeperCfg = "${secretsDir}/keeper.toml";
  metricsSnapshot = "${cfg.stateDir}/keeper-metrics-snapshot.json";

  # One pass over the deployment -> Prometheus text exposition. Never `set -e`: a single
  # dead source must not blank every other metric. The keeper balance is read through a
  # private snapshot so it never contends with the running daemon's snapshot.
  collect = pkgs.writeShellApplication {
    name = "styx-collect-metrics";
    runtimeInputs = [ elementsd styx pkgs.curl pkgs.jq pkgs.gnugrep pkgs.gnused pkgs.coreutils pkgs.systemd ];
    text = ''
      set -u
      D="-datadir=/var/lib/styxnet"
      OUT="${textfileDir}/styx.prom"
      TMP="$OUT.$$"
      cli(){ elements-cli $D "$@" 2>/dev/null; }
      now=$(date +%s)
      jnum(){ jq -r "$1 // empty" 2>/dev/null; }   # numeric field or nothing

      {
        # ---- Liquid node ----
        if info=$(cli getblockchaininfo) && [ -n "$info" ]; then
          echo "styx_node_up 1"
          b=$(jnum '.blocks' <<<"$info");                 [ -n "$b" ] && echo "styx_node_blocks $b"
          h=$(jnum '.headers' <<<"$info");                [ -n "$h" ] && echo "styx_node_headers $h"
          echo "styx_node_ibd $(jq -r 'if .initialblockdownload then 1 else 0 end' <<<"$info")"
          vp=$(jnum '.verificationprogress' <<<"$info");  [ -n "$vp" ] && echo "styx_node_verification_progress $vp"
          p=$(cli getconnectioncount);                    [ -n "$p" ] && echo "styx_node_peers $p"
          bh=$(cli getbestblockhash)
          if [ -n "$bh" ]; then
            t=$(cli getblockheader "$bh" | jnum '.time')
            [ -n "$t" ] && echo "styx_node_tip_age_seconds $(( now - t ))"
          fi
        else
          echo "styx_node_up 0"
        fi

        # ---- relay (NIP-11 over the ws port's http) ----
        if curl -sS -m 4 -o /dev/null -H 'Accept: application/nostr+json' http://127.0.0.1:${toString relayPort}; then
          echo "styx_relay_up 1"
        else
          echo "styx_relay_up 0"
        fi

        # ---- five oracles ----
        for slot in 0 1 2 3 4; do
          o=$(curl -sS -m 4 "http://127.0.0.1:$((${toString oracleBase}+slot))/health" 2>/dev/null) || continue
          [ -z "$o" ] && continue
          src=$(jq -r '.source // "unknown"' <<<"$o")
          echo "styx_oracle_publishing{slot=\"$slot\",source=\"$src\"} $(jq -r 'if .publishing then 1 else 0 end' <<<"$o")"
          v=$(jnum '.price' <<<"$o");         [ -n "$v" ] && echo "styx_oracle_price_usd{slot=\"$slot\"} $v"
          v=$(jnum '.height' <<<"$o");        [ -n "$v" ] && echo "styx_oracle_height{slot=\"$slot\"} $v"
          v=$(jnum '.feed_age_secs' <<<"$o"); [ -n "$v" ] && echo "styx_oracle_feed_age_seconds{slot=\"$slot\"} $v"
        done

        # ---- protocol + live tick + explorer, from the read-only explorer API ----
        if st=$(curl -sS -m 4 http://127.0.0.1:${toString explorerPort}/api/state 2>/dev/null) && [ -n "$st" ]; then
          echo "styx_explorer_up 1"
          v=$(jnum '.protocol.pot_units' <<<"$st");     [ -n "$v" ] && echo "styx_protocol_pot_obol $v"
          v=$(jnum '.protocol.reserve_sats' <<<"$st");  [ -n "$v" ] && echo "styx_protocol_reserve_sats $v"
          v=$(jnum '.protocol.issuer_anchor' <<<"$st"); [ -n "$v" ] && echo "styx_protocol_issuer_anchor $v"
          v=$(jnum '.height' <<<"$st");                 [ -n "$v" ] && echo "styx_explorer_height $v"
          v=$(jnum '.tick.lo' <<<"$st");                [ -n "$v" ] && echo "styx_tick_lo_usd $v"
          v=$(jnum '.tick.hi' <<<"$st");                [ -n "$v" ] && echo "styx_tick_hi_usd $v"
          v=$(jq -r '.vaults | length' <<<"$st" 2>/dev/null); [ -n "$v" ] && echo "styx_protocol_vaults $v"
        else
          echo "styx_explorer_up 0"
        fi

        # ---- keeper balances (private snapshot, no contention with the daemon) ----
        if [ -r "${keeperCfg}" ]; then
          kc="$RUNTIME_DIRECTORY/keeper-metrics.toml"
          sed 's#^snapshot *=.*#snapshot = "${metricsSnapshot}"#' "${keeperCfg}" > "$kc" 2>/dev/null || true
          if ks=$(styx-wallet --config "$kc" status 2>/dev/null) && [ -n "$ks" ]; then
            v=$(grep -oE 'L-BTC: [0-9]+' <<<"$ks" | grep -oE '[0-9]+' | head -1); [ -n "$v" ] && echo "styx_keeper_lbtc_sats $v"
            v=$(grep -oE 'OBOL: [0-9]+'  <<<"$ks" | grep -oE '[0-9]+' | head -1); [ -n "$v" ] && echo "styx_keeper_obol_units $v"
          fi
        fi

        # ---- keeper activity counters (from the journal; FEE is a fixed 10_000 sats/action) ----
        if jl=$(journalctl -u styx-keeper --no-pager -o cat 2>/dev/null); then
          echo "styx_keeper_pokes_total $(grep -c 'performed: Poked' <<<"$jl" || true)"
          echo "styx_keeper_refreshes_total $(grep -c 'performed: Refreshed' <<<"$jl" || true)"
          echo "styx_keeper_liquidations_total $(grep -cE 'performed: (FullLiq|Partial|BadDebt)' <<<"$jl" || true)"
        fi
      } > "$TMP" 2>/dev/null
      mv "$TMP" "$OUT"
    '';
  };
in
{
  options.services.styx-testnet.monitoring = {
    enable = lib.mkEnableOption "Prometheus + Grafana for the STYX stack";
    grafanaPort = lib.mkOption {
      type = lib.types.port;
      default = 3000;
      description = "Grafana HTTP port (loopback; front with an onion service or SSH tunnel).";
    };
  };

  config = lib.mkIf (cfg.enable && mcfg.enable) {
    systemd.tmpfiles.rules = [
      "d ${textfileDir} 0755 styx styx - -"
    ];

    systemd.services.styx-collect-metrics = {
      description = "Collect STYX metrics into the node-exporter textfile";
      after = [ "styx-elementsd.service" ];
      serviceConfig = {
        Type = "oneshot";
        User = "styx";
        Group = "styx";
        RuntimeDirectory = "styx-metrics";
        ExecStart = "${collect}/bin/styx-collect-metrics";
      };
    };
    systemd.timers.styx-collect-metrics = {
      description = "Refresh STYX metrics every 30s";
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "1min";
        OnUnitActiveSec = "30s";
      };
    };

    # node-exporter: host metrics + our textfile + systemd unit states (loopback only).
    services.prometheus.exporters.node = {
      enable = true;
      listenAddress = "127.0.0.1";
      port = nodeExporterPort;
      enabledCollectors = [ "systemd" "textfile" ];
      extraFlags = [ "--collector.textfile.directory=${textfileDir}" ];
    };

    services.prometheus = {
      enable = true;
      listenAddress = "127.0.0.1";
      port = promPort;
      retentionTime = "90d";
      globalConfig.scrape_interval = "30s";
      scrapeConfigs = [{
        job_name = "styx";
        static_configs = [{ targets = [ "127.0.0.1:${toString nodeExporterPort}" ]; }];
      }];
    };

    services.grafana = {
      enable = true;
      settings = {
        server = {
          http_addr = "127.0.0.1";
          http_port = grafanaPort;
          domain = "localhost";
        };
        analytics = {
          reporting_enabled = false;
          check_for_updates = false;
        };
        users.default_theme = "dark";
      };
      provision = {
        enable = true;
        datasources.settings = {
          apiVersion = 1;
          datasources = [{
            name = "Prometheus";
            type = "prometheus";
            access = "proxy";
            url = "http://127.0.0.1:${toString promPort}";
            isDefault = true;
          }];
        };
        dashboards.settings = {
          apiVersion = 1;
          providers = [{
            name = "styx";
            type = "file";
            updateIntervalSeconds = 30;
            options.path = "/etc/grafana-dashboards";
            options.foldersFromFilesStructure = false;
          }];
        };
      };
    };

    environment.etc."grafana-dashboards/styx.json".source = ./styx-dashboard.json;
  };
}
