//! CLI output contracts. These helpers do not open archives or state.
use super::*;

pub(super) fn safe_text(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

pub(super) fn print_test_report(report: &smartzip_archive::integrity::TestArchiveReport) {
    use smartzip_archive::integrity::{Integrity, SuspectRelation};
    println!(
        "{}: {} (coverage={}, localization={}, password={})",
        safe_text(&report.entrypoint.to_string_lossy()),
        enum_text(&report.integrity),
        enum_text(&report.coverage),
        enum_text(&report.localization),
        enum_text(&report.password_status)
    );
    for volume in &report.confirmed_volumes {
        println!(
            "  Confirmed damaged: {}",
            safe_text(&volume.path.to_string_lossy())
        );
        for evidence in report
            .evidence
            .iter()
            .filter(|e| volume.evidence_ids.contains(&e.id))
        {
            println!(
                "    {} [{}]",
                safe_text(&evidence.summary),
                safe_text(&evidence.source)
            );
        }
    }
    for (index, group) in report.suspect_groups.iter().enumerate() {
        println!(
            "  Suspected group {} ({}): {}",
            index + 1,
            match group.relation {
                SuspectRelation::OneOrMore => "one or more may be damaged",
                SuspectRelation::Possible => "possible; exact range unknown",
            },
            group
                .members
                .iter()
                .map(|p| safe_text(&p.to_string_lossy()))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for evidence in report
            .evidence
            .iter()
            .filter(|e| group.evidence_ids.contains(&e.id))
        {
            println!(
                "    {} [{}]",
                safe_text(&evidence.summary),
                safe_text(&evidence.source)
            );
        }
    }
    for (label, paths) in [
        ("Missing", &report.missing_volumes),
        ("Unreadable", &report.unreadable_volumes),
    ] {
        for path in paths {
            println!("  {label}: {}", safe_text(&path.to_string_lossy()));
        }
    }
    if !report.unchecked_volumes.is_empty() {
        println!(
            "  Full-volume health unchecked: {} volume(s)",
            report.unchecked_volumes.len()
        );
    }
    for reason in &report.stop_reasons {
        println!("  Note: {}", safe_text(reason));
    }
    if report.integrity != Integrity::Intact {
        println!("  Next: restore missing/unreadable volumes, replace confirmed damaged volumes, then test again; suspected members need further checking.");
    }
}

pub(super) fn enum_text(value: &impl std::fmt::Debug) -> String {
    format!("{value:?}")
        .chars()
        .enumerate()
        .flat_map(|(index, c)| {
            if c.is_uppercase() && index > 0 {
                vec!['_', c.to_ascii_lowercase()]
            } else {
                vec![c.to_ascii_lowercase()]
            }
        })
        .collect()
}

pub(super) fn print_detect_result(
    result: &FileAwareDetectResult,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
        return Ok(());
    }

    println!(
        "{} [{}] status={} embedded={} encrypted={} task-id={}",
        result.path.display(),
        result
            .detected_format
            .as_ref()
            .map(|fmt| fmt.as_str())
            .unwrap_or("unknown"),
        result.status,
        result.embedded_count,
        match result.encrypted {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        },
        result.task_id,
    );
    if let Some(encoding) = &result.encoding {
        if let Some(confidence) = result.encoding_confidence {
            println!("encoding: {encoding} ({:.0}%)", confidence * 100.0);
        } else {
            println!("encoding: {encoding}");
        }
    }
    if let Some(reason) = &result.reason {
        println!("reason: {reason}");
    }
    if result.needs_password {
        println!("password: required to continue");
    }
    if result.known_password {
        println!("known password: available");
    }
    if let Some(known_encoding) = &result.known_encoding {
        println!("known encoding: {known_encoding}");
    }
    if !result.embedded_findings.is_empty() {
        println!("embedded findings:");
        for finding in &result.embedded_findings {
            println!(
                "  - {} @ 0x{:X} size={} confidence={:?} {}",
                finding.format.as_str(),
                finding.offset,
                finding
                    .size
                    .map(|size| size.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                finding.confidence,
                finding.description,
            );
        }
    }
    Ok(())
}

pub(super) fn render_extract_event(event: &smartzip_core::TaskEvent, verbose_routing: bool) {
    match &event.kind {
        smartzip_core::TaskEventKind::Decision {
            stage,
            action,
            reason,
            policy_key,
            source,
            ..
        } if stage != "task_policy" && (*action == "skip" || verbose_routing) => {
            eprintln!("{stage}: {action} ({reason}; {policy_key}, {source})")
        }
        smartzip_core::TaskEventKind::Progress(progress) => match progress.percent {
            Some(percent) => eprintln!("  {percent:>3.0}%  {}", progress.message),
            None => eprintln!("  {}", progress.message),
        },
        smartzip_core::TaskEventKind::EncodingDetected(detection) => {
            let encoding = match &detection.selected {
                smartzip_core::EncodingMode::Auto => "auto",
                smartzip_core::EncodingMode::Override(s) => s.as_str(),
            };
            eprintln!(
                "  encoding: {encoding} (confidence: {:.0}%)",
                detection.confidence * 100.0
            );
        }
        smartzip_core::TaskEventKind::EmbeddedArchiveSelectionRequired {
            path,
            findings_count,
        } => {
            eprintln!(
                "  embedded selection required: {} ({} finding(s))",
                path.display(),
                findings_count
            );
        }
        smartzip_core::TaskEventKind::LargeEmbeddedScanConfirmationRequired {
            path,
            file_size,
            threshold,
        } => {
            eprintln!(
                "  large embedded scan skipped without confirmation: {} ({} bytes > {} bytes)",
                path.display(),
                file_size,
                threshold
            );
        }
        smartzip_core::TaskEventKind::BusinessContainerSkipped { path, kind } => {
            eprintln!("  skipped business container {kind}: {}", path.display());
        }
        smartzip_core::TaskEventKind::OutputCreated { path } => {
            eprintln!("  -> {}", path.display());
        }
        smartzip_core::TaskEventKind::Route(route) if verbose_routing => {
            render_route_event(route, true);
        }
        smartzip_core::TaskEventKind::Failed { error } => eprintln!("  FAILED: {error}"),
        smartzip_core::TaskEventKind::Warning { message } => {
            eprintln!("  warning: {message}")
        }
        _ => {}
    }
}

pub(super) fn render_route_event(route: &smartzip_core::RouteEvent, stderr: bool) {
    macro_rules! output {
        ($($args:tt)*) => {
            if stderr {
                eprintln!($($args)*);
            } else {
                println!($($args)*);
            }
        };
    }

    match route {
        smartzip_core::RouteEvent::RoutePlanned { plan } => {
            output!("  route: {:?}", plan.operation);
            for candidate in &plan.candidates {
                output!("    candidate: {}", candidate.adapter_id);
                for note in &candidate.notes {
                    output!("      note: {note}");
                }
            }
            for rejected in &plan.rejected {
                output!(
                    "    rejected: {} ({})",
                    rejected.adapter_id,
                    rejected.reasons.join("; ")
                );
            }
        }
        smartzip_core::RouteEvent::BackendAttemptStarted { adapter_id } => {
            output!("  route: trying {adapter_id}")
        }
        smartzip_core::RouteEvent::BackendAttemptFailed { adapter_id, class } => {
            output!("  route: {adapter_id} failed ({class})")
        }
        smartzip_core::RouteEvent::BackendAttemptCleaned { adapter_id } => {
            output!("  route: cleaned {adapter_id} output")
        }
        smartzip_core::RouteEvent::BackendSelected { adapter_id } => {
            output!("  route: selected {adapter_id}")
        }
        smartzip_core::RouteEvent::RouteExhausted { attempted } => {
            output!("  route: exhausted [{}]", attempted.join(", "))
        }
    }
}

pub(super) fn build_extract_json_output(
    result: &smartzip_engine::ExtractWorkflowResult,
) -> serde_json::Value {
    serde_json::json!({
        "task_id": result.task_id,
        "status": result.status,
        "failed_count": result.failed_count,
        "processed_count": result.processed.len(),
        "skipped_count": result.skipped.len(),
        "enqueued_count": result.enqueued.len(),
        "processed": result.processed,
        "skipped": result.skipped,
        "enqueued": result.enqueued,
        "events": result.events,
        "exit_code": result.status.exit_code(),
    })
}

pub(super) fn print_history_test_report(json: Option<&str>) {
    if let Some(json) = json {
        match serde_json::from_str::<smartzip_archive::integrity::TestArchiveReport>(json) {
            Ok(report) => print_test_report(&report),
            Err(_) => eprintln!("  test report has an unsupported or invalid schema"),
        }
    }
}

pub(super) fn print_list_result(
    result: &smartzip_engine::ListArchiveResult,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "{} [{}] enc={} password={} task-id={}",
            result.path.display(),
            result
                .detected_format
                .as_ref()
                .map(|fmt| fmt.as_str())
                .unwrap_or("unknown"),
            result.encoding,
            if result.used_password { "yes" } else { "no" },
            result.task_id,
        );
        for entry in &result.entries {
            let suffix = if entry.is_dir {
                "/"
            } else {
                Default::default()
            };
            println!("{}{}", entry.path.display(), suffix);
        }
    }
    Ok(())
}

pub(super) fn print_test_result(
    result: &smartzip_engine::TestWorkflowResult,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        for report in &result.files {
            print_test_report(report);
        }
        println!("task-id: {}", result.task_id);
    }
    Ok(())
}

pub(super) fn print_extract_result(
    result: &smartzip_engine::ExtractWorkflowResult,
    json: bool,
    show_task_id: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&build_extract_json_output(&result))?
        );
    } else {
        let processed_count = result.processed.len();
        let skipped_count = result.skipped.len();
        if processed_count > 0 {
            println!("processed {} archive(s)", processed_count);
        }
        if skipped_count > 0 {
            println!("skipped {} candidate(s)", skipped_count);
            for skipped in &result.skipped {
                println!("  - {} (depth {})", skipped.path.display(), skipped.depth);
            }
        }
        if show_task_id {
            println!("task-id: {}", result.task_id);
        }
    }

    Ok(())
}
