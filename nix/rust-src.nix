# Cargo workspace only. NixOS modules, Ansible, and docs must not
# rebuild voie-cloud/voie-fabricd or the guest images.
{ lib }:
lib.fileset.toSource {
  root = ../.;
  fileset = lib.fileset.unions [
    ../Cargo.toml
    ../Cargo.lock
    ../rust-toolchain.toml
    ../crates
  ];
}
