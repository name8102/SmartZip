//! Real backend regression tests. All archives and recycled files are temporary.
use smartzip_archive::{AdapterRegistration, BackendRouter, SevenZipBackend, SevenZipLocator};
use smartzip_config::{Cleanup, Conflict, Layout, ResolvedConfig, SmartZipConfig};
use smartzip_core::{EncodingMode, TaskEventKind};
use smartzip_db::{password::PasswordRepository, SmartZipDb};
use smartzip_engine::{CompiledRunPolicy, ExtractWorkflowRequest, SmartZipEngine};
use smartzip_passwords::{PasswordCandidateRequest, PasswordService};
use std::{
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

fn zip_bytes(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in files {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn inner_members(root: &Path, volumes: bool, leaf: &str, payload: &[u8]) -> Vec<PathBuf> {
    let inputs = root.join("inputs");
    let archives = root.join("archives");
    std::fs::create_dir(&inputs).unwrap();
    std::fs::create_dir(&archives).unwrap();
    let inner = archives.join("payload.zip");
    if volumes {
        std::fs::write(inputs.join(leaf), payload).unwrap();
        let created = std::process::Command::new("zip")
            .current_dir(&inputs)
            .args(["-0", "-q", "-s", "64k"])
            .arg(&inner)
            .arg(leaf)
            .output()
            .expect("zip is required for native split ZIP acceptance");
        assert!(created.status.success(), "{created:?}");
    } else {
        std::fs::write(&inner, zip_bytes(&[(leaf.into(), payload.to_vec())])).unwrap();
    }
    let mut members: Vec<_> = std::fs::read_dir(archives)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    members.sort();
    assert_eq!(members.len() > 1, volumes);
    members
}

async fn check_cleanup(volumes: bool, same_name: bool, change_source: bool, cleanup: Cleanup) {
    let root = tempfile::tempdir().unwrap();
    // Incompressible data makes multiple native split ZIP members deterministic.
    let payload: Vec<_> = (0..60_000u32)
        .flat_map(|n| n.wrapping_mul(2654435761).to_le_bytes())
        .collect();
    let leaf = if same_name { "payload.zip" } else { "leaf.bin" };
    let members = inner_members(root.path(), volumes, leaf, &payload);
    let mut outer_files: Vec<_> = members
        .iter()
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(p).unwrap(),
            )
        })
        .collect();
    outer_files.push(("readme.txt".into(), b"readme".to_vec()));
    let outer = root.path().join("bundle.zip");
    std::fs::write(&outer, zip_bytes(&outer_files)).unwrap();
    let output = root.path().join("out");
    let nested_root = output.join("bundle");
    let mut values = SmartZipConfig::default();
    values.extraction.output.layout = Layout::FlatSingle;
    values.extraction.output.on_conflict = Conflict::Overwrite;
    values.extraction.cleanup.nested_archives = cleanup;
    values.extraction.recursion.max_depth = 1;
    values.extraction.encoding.mode = "auto".into();
    values.extraction.embedded.root = smartzip_config::RootScan::Off;
    values.extraction.embedded.nested = smartzip_config::NestedScan::Off;
    values.interaction.mode = smartzip_config::InteractionMode::Never;
    let policy = CompiledRunPolicy::compile(ResolvedConfig {
        values,
        origins: Default::default(),
        path: None,
        diagnostics: vec![],
    })
    .unwrap();
    let recycled = Arc::new(Mutex::new(Vec::new()));
    let observed = recycled.clone();
    let engine = SmartZipEngine::default()
        .with_run_policy(policy)
        .with_archive_recycler(Arc::new(move |path| {
            observed.lock().unwrap().push(path.clone());
            std::fs::remove_file(path)
        }));
    let db = SmartZipDb::in_memory().unwrap();
    let passwords = PasswordService::new(PasswordRepository::new(db.connection()));
    let backend = BackendRouter::from_adapters(vec![AdapterRegistration::from_adapter(
        SevenZipBackend::locate(&SevenZipLocator::default()).unwrap(),
        10,
    )]);
    let changed = nested_root.join(members[0].file_name().unwrap());
    let changed_by_listener = changed.clone();
    let listener = change_source.then(|| {
        Arc::new(move |event: &smartzip_core::TaskEvent| {
            if matches!(&event.kind, TaskEventKind::OutputCreated { path }
                if path.file_name().is_some_and(|name| name == "leaf.bin"))
            {
                std::fs::write(&changed_by_listener, b"external replacement").unwrap();
            }
        }) as smartzip_engine::TaskEventListener
    });
    let request = ExtractWorkflowRequest {
        inputs: vec![outer.clone()],
        output_dir: output,
        recursion_limit: 1,
        encoding_mode: EncodingMode::Auto,
        scanner: Default::default(),
        password_candidates: PasswordCandidateRequest::default(),
        layout_policy: Default::default(),
        single_root_name_policy: Default::default(),
        embedded_scan_mode: Default::default(),
        dominant_min_ratio: 0.7,
        confirm_large_scan: false,
        force: false,
        limits: Default::default(),
    };
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        engine.extract_recursive_with_listener(&backend, &passwords, request, None, None, listener),
    )
    .await
    .expect("nested extraction must finish within the test bound")
    .unwrap();
    assert_eq!(result.failed_count, 0, "{:?}", result.events);
    assert_eq!(result.processed.len(), 2, "{:?}", result.events);
    assert_eq!(std::fs::read(nested_root.join(leaf)).unwrap(), payload);
    assert!(nested_root.join("readme.txt").exists());
    assert!(outer.exists());
    assert!(result.events.iter().all(|event| !matches!(&event.kind,
        TaskEventKind::Warning { message } if message.contains("backup retained"))));
    let mut expected_recycled = Vec::new();
    for member in members {
        let path = nested_root.join(member.file_name().unwrap());
        let protected =
            same_name && path.file_name().unwrap() == leaf || change_source && path == changed;
        let retained = cleanup == Cleanup::Keep || protected;
        assert_eq!(path.exists(), retained, "{cleanup:?}: {}", path.display());
        if cleanup == Cleanup::Trash && !retained {
            expected_recycled.push(path);
        }
    }
    let mut actual = recycled.lock().unwrap().clone();
    actual.sort();
    expected_recycled.sort();
    assert_eq!(actual, expected_recycled, "{cleanup:?}");
    if change_source {
        assert_eq!(std::fs::read(changed).unwrap(), b"external replacement");
    }
}

#[tokio::test]
async fn nested_cleanup_preserves_same_named_published_file_for_single_and_volume_inputs() {
    for volumes in [false, true] {
        for cleanup in [Cleanup::Keep, Cleanup::Delete, Cleanup::Trash] {
            check_cleanup(volumes, true, false, cleanup).await;
        }
    }
}

#[tokio::test]
async fn nested_cleanup_respects_keep_delete_trash_and_preserves_changed_sources() {
    for volumes in [false, true] {
        for cleanup in [Cleanup::Keep, Cleanup::Delete, Cleanup::Trash] {
            check_cleanup(volumes, false, false, cleanup).await;
            check_cleanup(volumes, false, true, cleanup).await;
        }
    }
}
