//! Real backend acceptance. The injected recycler removes only generated test files.
use smartzip_archive::{AdapterRegistration, BackendRouter, SevenZipBackend, SevenZipLocator};
use smartzip_core::EncodingMode;
use smartzip_db::{password::PasswordRepository, SmartZipDb};
use smartzip_engine::{ExtractWorkflowRequest, SmartZipEngine};
use smartzip_passwords::{PasswordCandidateRequest, PasswordService};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[tokio::test]
async fn successful_source_recycling_handles_single_first_middle_and_all_volumes() {
    for format in ["zip", "7z", "split-zip"] {
        for selection in ["single", "first", "middle", "all"] {
            let temp = tempfile::tempdir().unwrap();
            let payload: Vec<u8> = (0..60_000u32)
                .flat_map(|n| n.wrapping_mul(2654435761).to_le_bytes())
                .collect();
            std::fs::write(temp.path().join("payload.bin"), &payload).unwrap();
            let adapter = SevenZipBackend::locate(&SevenZipLocator::default()).unwrap();
            let mut command =
                std::process::Command::new(if format == "split-zip" { "zip" } else { "7z" });
            command.current_dir(temp.path());
            if format == "split-zip" {
                command.args(["-0", "-q"]);
                if selection != "single" {
                    command.args(["-s", "64k"]);
                }
            } else {
                command.args(["a", &format!("-t{format}"), "-mx=0"]);
                if selection != "single" {
                    command.arg("-v64k");
                }
            }
            let name = format!(
                "archive.{}",
                if format == "split-zip" { "zip" } else { format }
            );
            let creation = command.args([&name, "payload.bin"]).output().unwrap();
            assert!(creation.status.success());
            let mut members: Vec<PathBuf> = std::fs::read_dir(temp.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("archive.")
                })
                .collect();
            members.sort();
            let inputs = match selection {
                "all" => members.clone(),
                "middle" => vec![members[1].clone()],
                _ => vec![members[0].clone()],
            };
            let unrelated = temp.path().join("unrelated.zip");
            std::fs::write(&unrelated, b"unrelated").unwrap();
            let recycled = Arc::new(Mutex::new(Vec::new()));
            let observed = recycled.clone();
            let engine = SmartZipEngine::default()
                .with_source_recycling(true)
                .with_archive_recycler(Arc::new(move |path| {
                    observed.lock().unwrap().push(path.clone());
                    std::fs::remove_file(path)
                }));
            let db = SmartZipDb::in_memory().unwrap();
            let passwords = PasswordService::new(PasswordRepository::new(db.connection()));
            let backend =
                BackendRouter::from_adapters(vec![AdapterRegistration::from_adapter(adapter, 10)]);
            let mut request = request(inputs, temp.path().join("out"));
            if format == "split-zip" {
                request.encoding_mode = EncodingMode::Auto;
            }
            let mut limited = request.clone();
            limited.limits.max_output_bytes = 1;
            let failed = engine
                .extract_recursive(&backend, &passwords, limited, None, None)
                .await
                .unwrap();
            assert!(failed.failed_count > 0);
            assert!(members.iter().all(|p| p.exists()));
            assert!(recycled.lock().unwrap().is_empty());
            let result = engine
                .extract_recursive(&backend, &passwords, request, None, None)
                .await
                .unwrap();
            assert_eq!(
                result.failed_count, 0,
                "{format}/{selection}: {:?}",
                result.events
            );
            let mut actual = recycled.lock().unwrap().clone();
            actual.sort();
            assert_eq!(actual, members, "{format}/{selection}: {:?}", result.events);
            assert!(members.iter().all(|p| !p.exists()));
            assert!(unrelated.exists());
            let output = walkdir::WalkDir::new(temp.path().join("out"))
                .into_iter()
                .filter_map(Result::ok)
                .find(|e| e.file_name() == "payload.bin")
                .unwrap();
            assert_eq!(std::fs::read(output.path()).unwrap(), payload);
        }
    }
}

fn request(inputs: Vec<PathBuf>, output_dir: PathBuf) -> ExtractWorkflowRequest {
    ExtractWorkflowRequest {
        inputs,
        output_dir,
        recursion_limit: 0,
        encoding_mode: EncodingMode::Override("UTF-8".into()),
        scanner: Default::default(),
        password_candidates: PasswordCandidateRequest {
            include_empty: true,
            limit: 0,
            ..Default::default()
        },
        layout_policy: Default::default(),
        single_root_name_policy: Default::default(),
        embedded_scan_mode: Default::default(),
        dominant_min_ratio: 0.7,
        confirm_large_scan: false,
        force: false,
        limits: Default::default(),
    }
}

#[tokio::test]
async fn discarded_volume_hypothesis_is_not_recycled() {
    let root = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..30_000u32)
        .flat_map(|n| n.wrapping_mul(2654435761).to_le_bytes())
        .collect();
    std::fs::write(root.path().join("payload.bin"), &payload).unwrap();
    let creation = std::process::Command::new("7z")
        .current_dir(root.path())
        .args(["a", "-t7z", "-mx=0", "-v80k", "original.7z", "payload.bin"])
        .output()
        .unwrap();
    assert!(creation.status.success());
    let volumes = root.path().join("volumes");
    std::fs::create_dir(&volumes).unwrap();
    let seed = volumes.join("a1b1.bin");
    let bad = volumes.join("a2b1.bin");
    let good = volumes.join("a1b2.bin");
    std::fs::copy(root.path().join("original.7z.001"), &seed).unwrap();
    let tail = std::fs::read(root.path().join("original.7z.002")).unwrap();
    std::fs::write(&bad, vec![0; tail.len()]).unwrap();
    std::fs::write(&good, tail).unwrap();
    let db = SmartZipDb::in_memory().unwrap();
    let passwords = PasswordService::new(PasswordRepository::new(db.connection()));
    let adapter = SevenZipBackend::locate(&SevenZipLocator::default()).unwrap();
    let backend =
        BackendRouter::from_adapters(vec![AdapterRegistration::from_adapter(adapter, 10)]);
    let engine = SmartZipEngine::default()
        .with_source_recycling(true)
        .with_archive_recycler(Arc::new(std::fs::remove_file));
    let result = engine
        .extract_recursive(
            &backend,
            &passwords,
            request(vec![seed.clone()], root.path().join("out")),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.failed_count, 0, "{:?}", result.events);
    assert!(!seed.exists());
    assert!(!good.exists());
    assert!(bad.exists());
    assert!(root.path().join("original.7z.001").exists());
}
