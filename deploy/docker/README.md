# Container images

Every image is built by nix from the flake, not from a Dockerfile: the same flake rev
produces bit-identical layers on any machine - no `apt-get update` drift, no base-image
tag races. The one exception is the Caddy TLS front in `compose.infra.yml` (pin it by
digest, see the comment there).

## Build and load

```bash
nix build .#oracle-image && docker load < result
nix build .#keeper-image && docker load < result
nix build .#tools-image && docker load < result      # styx-wallet + styx-deploy, no entrypoint
nix build .#elementsd-image && docker load < result
nix build .#relay-image && docker load < result
nix build .#monitor-image && docker load < result
```

Images are tagged with the flake's short git rev (`dirty` on an uncommitted tree): the tag
IS the provenance. To reproduce someone's image, check out their rev and build - the store
path and the layer digests match.

```bash
nix build .#oracle-image --print-out-paths   # same rev -> same path, on any machine
```

## Run

Configs are mounted, never baked in (they carry secrets and differ per deployment):

```bash
docker run -v $PWD/oracle.toml:/etc/styx/oracle.toml:ro -p 127.0.0.1:9700:9700 \
    styx-oracle:TAG --config /etc/styx/oracle.toml

docker run styx-oracle:TAG --keygen

docker run -v $PWD/wallet.toml:/etc/styx/wallet.toml:ro \
    styx-tools:TAG /bin/styx-wallet --config /etc/styx/wallet.toml status
```

The compose files here assemble the hosts:

- `compose.oracle.yml` - a node + one oracle (per oracle host)
- `compose.keeper.yml` - a node + the keeper (note the restart policy: exit 65 is the
  invariant-break alert and must stay down)
- `compose.infra.yml` - node, relay, Caddy TLS front, monitor

Each file's header lists the config mounts it expects; the templates live one directory
up. In-container daemons reach their node at `http://elementsd:18884`, so the mounted
`elements.conf` binds RPC on `0.0.0.0` with `rpcallowip` scoped to the compose subnet
declared in the file - never publish the RPC port itself.

## Users and permissions

The role daemons run as uid 65532, not root. Two consequences:

- A mounted config must be readable by that uid - `chmod 640` plus a group, or
  `chown 65532` the file. A root-only `oracle.toml` fails at startup with a permission
  error, which is the correct failure.
- State directories (`/var/lib/styx` in the keeper, `/var/lib/relay` in the relay) exist
  in the image owned by 65532, so the named volumes initialize writable. For `styx-tools`
  against a bind-mounted host directory, pass `--user` matching the mount's owner.

elementsd is the exception and runs as root: `/data` is a host-managed volume and its
ownership is the deployment's decision - chown the volume and add `user:` to the service
to drop privileges there too.
