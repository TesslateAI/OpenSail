# Cargo workspace inputs for guest images. Daemon crate sources are
# excluded so a voie-cloud / voie-fabricd change does not rebuild the
# 1.1GiB workspace image. Workspace manifests and crate roots stay so
# `cargo build -p voie-runner` (and the other guest packages) still
# resolves the workspace. Overlay rust-src.nix still hashes every crate.
{ lib }:
lib.fileset.toSource {
  root = ../.;
  fileset = lib.fileset.unions [
    ../Cargo.toml
    ../Cargo.lock
    ../rust-toolchain.toml
    ../crates/voie-runner
    ../crates/voie-pack
    ../crates/voie-app-init
    ../crates/voie-egress
    ../crates/voie-cloud/Cargo.toml
    ../crates/voie-cloud/src/lib.rs
    ../crates/voie-cloud/src/main.rs
    ../crates/voie-fabricd/Cargo.toml
    ../crates/voie-fabricd/src/lib.rs
    ../crates/voie-fabricd/src/main.rs
  ];
}
