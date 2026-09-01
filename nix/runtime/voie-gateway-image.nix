# Trusted Fabric gateway and Application egress proxy.
# Not a user workload; voie-fabricd generates routes and the proxy argv.
{
  dockerTools,
  caddy,
  cacert,
  pkgsStatic,
  lib,
}:
let
  egress = pkgsStatic.rustPlatform.buildRustPackage {
    pname = "voie-egress";
    version = "0.0.0";
    src = import ../guest-rust-src.nix { inherit lib; };
    cargoLock.lockFile = ../../Cargo.lock;
    buildAndTestSubdir = "crates/voie-egress";
    doCheck = false;
    meta = {
      description = "Platform Application HTTP CONNECT egress proxy";
      mainProgram = "voie-egress";
    };
  };
in
dockerTools.buildLayeredImage {
  name = "voie-gateway";
  tag = "v1";
  contents = [
    caddy
    cacert
    egress
  ];
  extraCommands = ''
    mkdir -p etc/caddy tmp bin
    ln -sfn ${caddy}/bin/caddy bin/caddy
  '';
  config = {
    Entrypoint = [ "${caddy}/bin/caddy" ];
    Cmd = [
      "run"
      "--config"
      "/etc/caddy/Caddyfile"
      "--adapter"
      "caddyfile"
    ];
    Env = [ "PATH=/bin" ];
  };
  meta = {
    description = "Deployment-owned Fabric Caddy gateway for Application routes";
  };
}
