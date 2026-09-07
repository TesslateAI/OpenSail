//! Shared X.509 v3 CA + client identity for rustls `Identity::from_pem`.
//!
//! `openssl req -x509` without extensions emits v1 certificates. rustls 0.23
//! rejects those (`UnsupportedCertVersion`) when building a client identity.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub struct ClientPems {
    pub ca_pem: PathBuf,
    pub client_pem: PathBuf,
    pub client_key: PathBuf,
}

pub struct MtlsPems {
    pub ca_pem: PathBuf,
    pub client_pem: PathBuf,
    pub client_key: PathBuf,
    pub server_pem: PathBuf,
    pub server_key: PathBuf,
}

pub fn write_v3_ca_and_client(dir: &Path) -> ClientPems {
    let ca_key = dir.join("ca.key");
    let ca_pem = dir.join("ca.pem");
    let client_key = dir.join("client.key");
    let client_csr = dir.join("client.csr");
    let client_pem = dir.join("client.pem");
    let ext = dir.join("client.ext");
    fs::write(
        &ext,
        "basicConstraints=CA:FALSE\n\
         keyUsage=digitalSignature,keyEncipherment\n\
         extendedKeyUsage=clientAuth\n",
    )
    .expect("client.ext");
    run(&[
        "openssl",
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        ca_key.to_str().unwrap(),
        "-out",
        ca_pem.to_str().unwrap(),
        "-days",
        "2",
        "-subj",
        "/CN=voie-test-ca",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
    ]);
    run(&[
        "openssl",
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        client_key.to_str().unwrap(),
        "-out",
        client_csr.to_str().unwrap(),
        "-subj",
        "/CN=voie-test-client",
    ]);
    run(&[
        "openssl",
        "x509",
        "-req",
        "-in",
        client_csr.to_str().unwrap(),
        "-CA",
        ca_pem.to_str().unwrap(),
        "-CAkey",
        ca_key.to_str().unwrap(),
        "-CAcreateserial",
        "-out",
        client_pem.to_str().unwrap(),
        "-days",
        "2",
        "-extfile",
        ext.to_str().unwrap(),
    ]);
    ClientPems {
        ca_pem,
        client_pem,
        client_key,
    }
}

pub fn write_v3_mtls_bundle(dir: &Path) -> MtlsPems {
    let client = write_v3_ca_and_client(dir);
    let ca_key = dir.join("ca.key");
    let server_key = dir.join("server.key");
    let server_csr = dir.join("server.csr");
    let server_pem = dir.join("server.pem");
    let san = dir.join("server-san.ext");
    fs::write(&san, "subjectAltName=IP:127.0.0.1,DNS:localhost").expect("SAN extension writes");
    run(&[
        "openssl",
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        server_key.to_str().unwrap(),
        "-out",
        server_csr.to_str().unwrap(),
        "-subj",
        "/CN=voie-test-fabric",
    ]);
    run(&[
        "openssl",
        "x509",
        "-req",
        "-in",
        server_csr.to_str().unwrap(),
        "-CA",
        client.ca_pem.to_str().unwrap(),
        "-CAkey",
        ca_key.to_str().unwrap(),
        "-CAcreateserial",
        "-out",
        server_pem.to_str().unwrap(),
        "-days",
        "2",
        "-extfile",
        san.to_str().unwrap(),
    ]);
    MtlsPems {
        ca_pem: client.ca_pem,
        client_pem: client.client_pem,
        client_key: client.client_key,
        server_pem,
        server_key,
    }
}

fn run(args: &[&str]) {
    let status = Command::new(args[0])
        .args(&args[1..])
        .status()
        .unwrap_or_else(|err| panic!("openssl {args:?}: {err}"));
    assert!(status.success(), "openssl {args:?} failed");
}
