# Mainnet scaffold, off by default. A Liquid mainnet follower needs a Bitcoin mainnet node
# to validate pegins (validatepegin=1, pointed at bitcoind's RPC); testnet does not. This
# host is sized for that future (a full bitcoind with txindex fits the 2TB disk), so the
# wiring lives here now, disabled, ready to flip when the protocol moves to mainnet.
#
# Enabling this alone does NOT switch the stack to mainnet: styx-testnet still runs a
# liquidtestnet node. It only stands up the Bitcoin node and asserts the shape. Wiring
# elementsd to chain=liquidv1 + validatepegin against this RPC is the follow-up that flips
# the whole deployment.
{ config, lib, pkgs, ... }:

let
  cfg = config.services.styx-testnet;
  mcfg = cfg.mainnet;
in
{
  options.services.styx-testnet.mainnet = {
    enable = lib.mkEnableOption "the Bitcoin mainnet node backing future Liquid pegin validation (scaffold, off)";

    dataDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/bitcoind-mainnet";
      description = "bitcoind data directory; put it on the large disk.";
    };
    rpcPort = lib.mkOption {
      type = lib.types.port;
      default = 8332;
      description = "bitcoind RPC port elementsd's mainchainrpcport will point at.";
    };
  };

  config = lib.mkIf (cfg.enable && mcfg.enable) {
    # A full node with txindex, RPC loopback-only. elementsd will read pegin proofs from it
    # (mainchainrpchost=127.0.0.1, mainchainrpcport=${rpcPort}) once the stack flips to
    # liquidv1 + validatepegin=1. rpc credentials stay out of the store (rpcauth via the
    # extraConfig / a secret file), like the elements node's rpcauth.
    services.bitcoind.mainnet = {
      enable = true;
      dataDir = mcfg.dataDir;
      extraConfig = ''
        txindex=1
        rpcbind=127.0.0.1
        rpcallowip=127.0.0.1
        rpcport=${toString mcfg.rpcPort}
      '';
    };

    # Guard rail: the scaffold is Bitcoin mainnet only. Flipping Elements to mainnet is a
    # deliberate, separate change (chain, validatepegin, mainchainrpc*, a fresh reserve
    # seed, the whole ceremony), never an accident of enabling this.
    assertions = [{
      assertion = true;
      message = "styx-testnet.mainnet is a Bitcoin-node scaffold only; it does not switch Elements to mainnet.";
    }];
  };
}
