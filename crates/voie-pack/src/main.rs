//! Guest helper: package the Application root and print hashes as JSON.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(first) = args.next() else {
        eprintln!(
            "usage: voie-pack release APPLICATION_ROOT [RELATIVE_ROOT]\n       voie-pack workspace-snapshot WORKSPACE_ROOT\n       voie-pack APPLICATION_ROOT [RELATIVE_ROOT]"
        );
        return ExitCode::from(2);
    };
    let result = match first.as_str() {
        "workspace-snapshot" => {
            let Some(root) = args.next() else {
                eprintln!("usage: voie-pack workspace-snapshot WORKSPACE_ROOT");
                return ExitCode::from(2);
            };
            voie_pack::snapshot_and_stage(&PathBuf::from(root))
        }
        "release" => {
            let Some(root) = args.next() else {
                eprintln!("usage: voie-pack release APPLICATION_ROOT [RELATIVE_ROOT]");
                return ExitCode::from(2);
            };
            let relative = args.next().unwrap_or_else(|| ".".to_owned());
            voie_pack::pack_and_stage(&PathBuf::from(root), &relative)
        }
        _ => {
            let relative = args.next().unwrap_or_else(|| ".".to_owned());
            voie_pack::pack_and_stage(&PathBuf::from(first), &relative)
        }
    };
    match result {
        Ok(result) => {
            let hash = result
                .artifact_hash
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let _ = writeln!(
                io::stdout(),
                "{{\"artifactHash\":\"{hash}\",\"fileCount\":{},\"byteLength\":{}}}",
                result.file_count,
                result.byte_length
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("voie-pack: {error}");
            ExitCode::from(1)
        }
    }
}
