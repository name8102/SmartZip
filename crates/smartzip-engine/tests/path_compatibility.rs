use smartzip_archive::BackendRouter;
use smartzip_core::{EmbeddedScanMode, EncodingMode, PathMode, TaskEventKind};
use smartzip_engine::{
    CompiledRunPolicy, ExtractInteraction, ExtractObserver, ExtractWorkflowRequest,
    PreparedExtractTask, SmartZipEngine,
};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

fn archive(path: &Path, entries: &[(String, Vec<u8>)]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for (name, bytes) in entries {
        if name.ends_with('/') {
            zip.add_directory(name, zip::write::SimpleFileOptions::default())
                .unwrap();
        } else {
            zip.start_file(
                name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            zip.write_all(bytes).unwrap();
        }
    }
    zip.finish().unwrap();
}

async fn run(
    input: PathBuf,
    output: PathBuf,
    mode: PathMode,
    byte_limit: u64,
) -> smartzip_engine::ExtractWorkflowResult {
    run_with_layout(
        input,
        output,
        mode,
        byte_limit,
        smartzip_config::Layout::Raw,
    )
    .await
}

async fn run_with_layout(
    input: PathBuf,
    output: PathBuf,
    mode: PathMode,
    byte_limit: u64,
    layout: smartzip_config::Layout,
) -> smartzip_engine::ExtractWorkflowResult {
    let mut values = smartzip_config::SmartZipConfig::default();
    values.state.mode = smartzip_config::StateMode::Off;
    values.extraction.recursion.enabled = false;
    values.extraction.embedded.root = smartzip_config::RootScan::Off;
    values.extraction.embedded.nested = smartzip_config::NestedScan::Off;
    values.extraction.volumes.auto_discover = false;
    values.extraction.output.path_mode = mode;
    values.extraction.output.destination = smartzip_config::Destination::Directory;
    values.extraction.output.directory = Some(output.clone());
    values.extraction.output.layout = layout;
    values.extraction.output.on_conflict = smartzip_config::Conflict::Overwrite;
    values.limits.max_output_bytes = byte_limit;
    let resolved = smartzip_config::ResolvedConfig {
        values,
        origins: Default::default(),
        path: None,
        diagnostics: vec![],
    };
    let backend = BackendRouter::from_config(&resolved.values.backends).unwrap();
    let policy = CompiledRunPolicy::compile(resolved).unwrap();
    let task = PreparedExtractTask::new(
        policy,
        ExtractWorkflowRequest {
            inputs: vec![input],
            output_dir: output,
            recursion_limit: 0,
            encoding_mode: EncodingMode::Auto,
            scanner: Default::default(),
            password_candidates: Default::default(),
            layout_policy: Default::default(),
            single_root_name_policy: Default::default(),
            embedded_scan_mode: EmbeddedScanMode::Ignore,
            dominant_min_ratio: 0.7,
            confirm_large_scan: false,
            force: true,
            limits: Default::default(),
        },
    )
    .unwrap();
    task.run(
        SmartZipEngine::default(),
        &backend,
        None,
        ExtractInteraction::default(),
        ExtractObserver::default(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn real_zip_preserves_every_content_and_complete_final_mapping() {
    let root = tempfile::tempdir().unwrap();
    let directory = "很长的共享中文目录".repeat(40);
    let names = vec![
        format!("{directory}/{}.txt", "中文".repeat(170)),
        format!("{directory}/{}.bin", "👨‍👩‍👧‍👦".repeat(60)),
        "Case.txt".into(),
        "case.txt".into(),
        "é.txt".into(),
        "e\u{301}.txt".into(),
        "CON.txt".into(),
        "bad:name?.txt".into(),
        "tail. ".into(),
        format!("extension.{}", "扩展".repeat(150)),
        "empty/".into(),
    ];
    let entries: Vec<_> = names
        .into_iter()
        .enumerate()
        .map(|(i, n)| {
            let body = if n.ends_with('/') {
                vec![]
            } else {
                format!("unique member {i}: 中文内容\n")
                    .repeat(i + 1)
                    .into_bytes()
            };
            (n, body)
        })
        .collect();
    let first = root.path().join("first.zip");
    archive(&first, &entries);
    let output = root.path().join("output");
    let result = run(first, output.clone(), PathMode::Portable, 0).await;
    assert_eq!(result.failed_count, 0, "{:?}", result.events);
    assert_eq!(result.path_reports.len(), 1);
    let report = &result.path_reports[0];
    assert!(!report.tentative);
    assert_eq!(report.entries.len(), entries.len());
    assert!(report.changed_count() >= 8);
    let original: BTreeMap<_, _> = entries.iter().map(|(n, b)| (n.as_str(), b)).collect();
    for entry in &report.entries {
        assert!(entry.raw_name.is_some());
        let actual = output.join(&entry.final_relative);
        for component in Path::new(&entry.staging_relative).components() {
            assert!(component.as_os_str().to_str().unwrap().len() <= 255);
        }
        if entry.is_dir {
            assert!(actual.is_dir());
        } else {
            let bytes = std::fs::read(actual).unwrap();
            assert_eq!(
                blake3::hash(&bytes),
                blake3::hash(original[entry.source.as_str()])
            );
        }
    }
    let mut reversed = entries.clone();
    reversed.reverse();
    let second = root.path().join("second.zip");
    archive(&second, &reversed);
    let other = run(second, root.path().join("other"), PathMode::Portable, 0).await;
    assert_eq!(other.failed_count, 0, "{:?}", other.events);
    let mapped = |report: &smartzip_core::PathMappingReport| {
        report
            .entries
            .iter()
            .map(|e| (e.source.clone(), e.staging_relative.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(mapped(report), mapped(&other.path_reports[0]));
    assert!(result
        .events
        .iter()
        .any(|e| matches!(e.kind, TaskEventKind::PathMappingApplied { .. })));
}

#[tokio::test]
async fn real_native_volume_accepts_or_remaps_long_chinese_and_emoji() {
    let root = tempfile::tempdir().unwrap();
    let entries = vec![
        (
            format!("{}.txt", "中文".repeat(170)),
            b"chinese original payload".to_vec(),
        ),
        (
            format!("{}.bin", "👩🏽‍🚀".repeat(80)),
            b"emoji original payload".to_vec(),
        ),
    ];
    let input = root.path().join("native.zip");
    archive(&input, &entries);
    let output = root.path().join("output");
    let result = run(input, output.clone(), PathMode::Native, 0).await;
    assert_eq!(result.failed_count, 0, "{:?}", result.events);
    assert_eq!(result.path_reports[0].entries.len(), 2);
    for entry in &result.path_reports[0].entries {
        let original = entries
            .iter()
            .find(|(name, _)| name == &entry.source)
            .unwrap();
        assert_eq!(
            std::fs::read(output.join(&entry.final_relative)).unwrap(),
            original.1
        );
    }
}

#[tokio::test]
async fn real_native_long_directory_and_leaf_keep_hierarchy_and_content() {
    let root = tempfile::tempdir().unwrap();
    let directory = "共享目录中文".repeat(80);
    let entries = vec![
        (
            format!("{directory}/{}.txt", "中文".repeat(170)),
            b"first full payload".to_vec(),
        ),
        (
            format!("{directory}/{}.bin", "👩🏽‍🚀".repeat(80)),
            b"second full payload".to_vec(),
        ),
    ];
    let input = root.path().join("nested.zip");
    archive(&input, &entries);
    let output = root.path().join("output");
    let result = run(input, output.clone(), PathMode::Native, 0).await;
    assert_eq!(result.failed_count, 0, "{:?}", result.events);
    let report = &result.path_reports[0];
    let parents: Vec<_> = report
        .entries
        .iter()
        .map(|e| {
            Path::new(&e.staging_relative)
                .parent()
                .unwrap()
                .to_path_buf()
        })
        .collect();
    assert_eq!(parents[0], parents[1]);
    assert!(!parents[0].as_os_str().is_empty());
    for entry in &report.entries {
        let original = entries
            .iter()
            .find(|(name, _)| name == &entry.source)
            .unwrap();
        assert_eq!(
            std::fs::read(output.join(&entry.final_relative)).unwrap(),
            original.1
        );
    }
}

#[tokio::test]
async fn managed_budget_failure_keeps_old_output_and_cleans_staging() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("budget.zip");
    archive(
        &input,
        &[(format!("{}.txt", "中文".repeat(170)), vec![42; 65536])],
    );
    let output = root.path().join("output");
    let old = output.join("budget");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("keep"), b"old user content").unwrap();
    let result = run(input, output.clone(), PathMode::Portable, 128).await;
    assert_eq!(result.failed_count, 1, "{:?}", result.events);
    assert_eq!(
        std::fs::read(old.join("keep")).unwrap(),
        b"old user content"
    );
    assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
    assert!(!result.path_reports.iter().any(|r| !r.tentative));
}

#[tokio::test]
async fn normalized_duplicate_is_rejected_before_publication() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("duplicate.zip");
    archive(
        &input,
        &[
            ("a/b.txt".into(), b"first".to_vec()),
            ("a/./b.txt".into(), b"second".to_vec()),
        ],
    );
    let output = root.path().join("output");
    let result = run(input, output.clone(), PathMode::Portable, 0).await;
    assert_eq!(result.failed_count, 1, "{:?}", result.events);
    assert!(result.events.iter().any(|e|matches!(&e.kind,TaskEventKind::PathConstraintFailed {reason,..} if reason=="name_collision")));
    assert!(!output.join("duplicate").exists());
    if output.exists() {
        assert_eq!(std::fs::read_dir(output).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn explicit_root_directory_aliases_remain_in_final_mapping() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("root.zip");
    archive(
        &input,
        &[
            ("./".into(), vec![]),
            ("././".into(), vec![]),
            ("single.txt".into(), b"complete root payload".to_vec()),
        ],
    );
    let output = root.path().join("output");
    let result = run_with_layout(
        input,
        output.clone(),
        PathMode::Portable,
        0,
        smartzip_config::Layout::Conservative,
    )
    .await;
    assert_eq!(result.failed_count, 0, "{:?}", result.events);
    let report = &result.path_reports[0];
    assert_eq!(report.entries.len(), 3);
    assert!(!report.tentative);
    for entry in report.entries.iter().filter(|entry| entry.is_dir) {
        assert!(entry.staging_relative.is_empty());
        assert_eq!(entry.final_relative, "root");
        assert!(output.join(&entry.final_relative).is_dir());
        assert_eq!(entry.raw_name.as_deref(), Some(entry.source.as_bytes()));
    }
    let entry = report.entries.iter().find(|entry| !entry.is_dir).unwrap();
    assert_eq!(
        std::fs::read(output.join(&entry.final_relative)).unwrap(),
        b"complete root payload"
    );
}

#[tokio::test]
async fn layout_folding_keeps_metadata_members_and_their_mapping() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("metadata.zip");
    archive(
        &input,
        &[
            ("single.txt".into(), b"main payload".to_vec()),
            ("__MACOSX/metadata".into(), b"metadata payload".to_vec()),
        ],
    );
    let output = root.path().join("output");
    let result = run_with_layout(
        input,
        output.clone(),
        PathMode::Portable,
        0,
        smartzip_config::Layout::Conservative,
    )
    .await;
    assert_eq!(result.failed_count, 0, "{:?}", result.events);
    let report = &result.path_reports[0];
    assert_eq!(report.entries.len(), 2);
    for entry in &report.entries {
        let expected = if entry.source == "single.txt" {
            b"main payload".as_slice()
        } else {
            b"metadata payload".as_slice()
        };
        assert_eq!(
            std::fs::read(output.join(&entry.final_relative)).unwrap(),
            expected
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn restrictive_directory_metadata_is_applied_after_scan_and_layout() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("permissions.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&input).unwrap());
    zip.add_directory(
        "./",
        zip::write::SimpleFileOptions::default().unix_permissions(0),
    )
    .unwrap();
    zip.add_directory(
        "private/",
        zip::write::SimpleFileOptions::default().unix_permissions(0o400),
    )
    .unwrap();
    zip.start_file("private/payload", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"complete protected content").unwrap();
    zip.finish().unwrap();
    let output = root.path().join("output");
    let result = run_with_layout(
        input,
        output.clone(),
        PathMode::Portable,
        0,
        smartzip_config::Layout::Conservative,
    )
    .await;
    assert_eq!(result.failed_count, 0, "{:?}", result.events);
    let report = &result.path_reports[0];
    let container = output.join("permissions");
    assert_eq!(
        std::fs::metadata(&container).unwrap().permissions().mode() & 0o777,
        0
    );
    std::fs::set_permissions(&container, std::fs::Permissions::from_mode(0o700)).unwrap();
    let directory = container.join("private");
    assert_eq!(
        std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o400
    );
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let entry = report.entries.iter().find(|entry| !entry.is_dir).unwrap();
    assert_eq!(
        std::fs::read(output.join(&entry.final_relative)).unwrap(),
        b"complete protected content"
    );
}
