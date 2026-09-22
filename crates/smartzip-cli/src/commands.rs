//! Command-specific request execution; rendering and exit policy remain explicit.
use super::*;

pub(super) async fn detect(
    backend: &BackendRouter,
    db: Option<&SmartZipDb>,
    request: DetectCommand,
    verbose_routing: bool,
    safety: &SafetyOptions,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let DetectCommand {
        path,
        deep,
        json,
        max_scan_bytes,
        min_confidence,
    } = request;

    let config = ScannerConfig {
        min_confidence: min_confidence.into(),
        ..scanner_config(deep, max_scan_bytes)
    };
    let engine = SmartZipEngine::with_scanner_config(config.clone())
        .with_cancellation_token(cancellation.clone())
        .with_run_policy(safety.policy.as_ref().unwrap().as_ref().clone());
    let (history_store, known_store) = task_stores(db, safety);
    let recorder = run_stores(&history_store, &known_store);
    let result = engine
        .inspect(
            backend,
            InspectRequest {
                path,
                scanner: config,
            },
            task_listener(json, verbose_routing, safety),
            (history_store.is_some() || known_store.is_some())
                .then_some(&recorder as &dyn smartzip_engine::history::TaskHistoryRecorder),
        )
        .await?;

    print_detect_result(&result, json)?;
    command_exit(if result.status == "unreadable" { 1 } else { 0 })
}

pub(super) async fn test_archives(
    backend: &BackendRouter,
    db: Option<&SmartZipDb>,
    request: TestCommand,
    verbose_routing: bool,
    safety: &SafetyOptions,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let TestCommand {
        paths,
        password,
        no_empty,
        encoding,
        json,
        deep,
        max_scan_bytes,
        min_confidence,
        diagnose,
        diagnostic_timeout,
        no_history,
        use_clipboard: _,
    } = request;
    let request = smartzip_engine::TestWorkflowRequest {
        paths,
        encoding: parse_encoding_mode(&encoding),
        scanner: ScannerConfig {
            min_confidence: min_confidence.into(),
            ..scanner_config(deep, max_scan_bytes)
        },
        password_candidates: PasswordCandidateRequest {
            manual: password,
            clipboard: None,
            include_empty: !no_empty,
            limit: safety.password_limit,
        },
        diagnose: match diagnose {
            DiagnoseArg::Auto => smartzip_engine::DiagnoseMode::Auto,
            DiagnoseArg::Off => smartzip_engine::DiagnoseMode::Off,
        },
        diagnostic_timeout: diagnostic_timeout.map(std::time::Duration::from_secs),
        control: smartzip_archive::diagnostic::DiagnosticControl::with_cancellation(
            cancellation.clone(),
        ),
    };

    let service = task_passwords(db, safety);
    let engine = SmartZipEngine::with_scanner_config(request.scanner.clone())
        .with_run_policy(safety.policy.as_ref().unwrap().as_ref().clone());
    let (history_store, known_store) = task_stores(db, safety);
    let recorder = run_stores(&history_store, &known_store);
    let prompter = StdinPrompter {
        lock: StdinLock::configured(cancellation.clone(), safety, json),
    };
    let level = safety.policy.as_ref().unwrap().values().logging.level;
    let listener = (!json && level != smartzip_config::LogLevel::Off).then(|| {
        Arc::new(move |event: &TaskEvent| match &event.kind {
            smartzip_core::TaskEventKind::TestPhase { path, phase, .. }
                if matches!(
                    level,
                    smartzip_config::LogLevel::Info | smartzip_config::LogLevel::Debug
                ) =>
            {
                eprintln!("{}: {}", safe_text(&path.to_string_lossy()), phase)
            }
            smartzip_core::TaskEventKind::Warning { message }
                if level != smartzip_config::LogLevel::Error =>
            {
                eprintln!("warning: {}", safe_text(message))
            }
            smartzip_core::TaskEventKind::Route(route) if verbose_routing => {
                render_route_event(route, false)
            }
            _ => {}
        }) as smartzip_engine::TaskEventListener
    });
    let result = engine
        .test_archives(
            backend,
            &service,
            request,
            if prompter.lock.interactive {
                Some(&prompter)
            } else {
                None
            },
            listener,
            if no_history { None } else { Some(&recorder) },
        )
        .await;
    let result = result?;
    print_test_result(&result, json)?;
    command_exit(result.exit_code)
}

pub(super) async fn list_archive(
    backend: &BackendRouter,
    db: Option<&SmartZipDb>,
    request: ListCommand,
    verbose_routing: bool,
    safety: &SafetyOptions,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let ListCommand {
        path,
        password: manual_passwords,
        no_empty,
        encoding,
        pick_encoding,
        json,
        deep,
        max_scan_bytes,
        min_confidence,
    } = request;

    let config = ScannerConfig {
        min_confidence: min_confidence.into(),
        ..scanner_config(deep, max_scan_bytes)
    };
    let service = task_passwords(db, safety);
    let engine = SmartZipEngine::with_scanner_config(config.clone())
        .with_cancellation_token(cancellation.clone())
        .with_run_policy(safety.policy.as_ref().unwrap().as_ref().clone());
    let (history_store, known_store) = task_stores(db, safety);
    let recorder = run_stores(&history_store, &known_store);
    let stdin_lock = StdinLock::configured(cancellation.clone(), safety, json);
    let password_prompter = StdinPrompter {
        lock: stdin_lock.clone(),
    };
    let encoding_mode =
        select_list_encoding(path.clone(), &encoding, pick_encoding, stdin_lock.clone()).await?;
    let result = engine
        .list_archive_with_listener_interactive(
            backend,
            &service,
            ListArchiveRequest {
                path,
                scanner: config,
                encoding_mode,
                password_candidates: PasswordCandidateRequest {
                    manual: manual_passwords,
                    clipboard: None,
                    include_empty: !no_empty,
                    limit: safety.password_limit,
                },
            },
            if stdin_lock.interactive {
                Some(&password_prompter)
            } else {
                None
            },
            Some(&StdinEncodingPrompter { lock: stdin_lock }),
            task_listener(json, verbose_routing, safety),
            (history_store.is_some() || known_store.is_some())
                .then_some(&recorder as &dyn smartzip_engine::history::TaskHistoryRecorder),
        )
        .await?;

    print_list_result(&result, json)?;
    Ok(())
}

pub(super) async fn extract(
    backend: &BackendRouter,
    db: Option<&SmartZipDb>,
    request: ExtractCommand,
    verbose_routing: bool,
    safety: &SafetyOptions,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let ExtractCommand {
        paths,
        output,
        recursion_limit,
        password: manual_passwords,
        no_empty,
        deep,
        max_scan_bytes,
        encoding,
        json,
        layout,
        single_root_name,
        dry_run: _,
        embedded,
        dominant_min_ratio,
        confirm_large_scan,
        force,
        no_history: _,
    } = request;
    let layout_policy = layout.into();
    let single_root_name_policy = single_root_name.into();

    if paths.is_empty() {
        return Err("no paths provided".into());
    }
    let paths = paths
        .into_iter()
        .map(std::path::absolute)
        .collect::<Result<Vec<_>, _>>()?;
    let output = output.map(std::path::absolute).transpose()?;

    let encoding_mode =
        if encoding.eq_ignore_ascii_case("auto") || encoding.eq_ignore_ascii_case("backend") {
            EncodingMode::Auto
        } else {
            EncodingMode::Override(encoding.to_string())
        };

    let output_dir = output.unwrap_or_else(|| default_output_dir(paths.first().unwrap()));
    let workflow_request = ExtractWorkflowRequest {
        inputs: paths.clone(),
        output_dir: output_dir.clone(),
        recursion_limit,
        scanner: scanner_config(deep, max_scan_bytes),
        encoding_mode,
        password_candidates: PasswordCandidateRequest {
            manual: manual_passwords,
            clipboard: None,
            include_empty: !no_empty,
            limit: safety.password_limit,
        },
        layout_policy,
        single_root_name_policy,
        embedded_scan_mode: embedded.into(),
        dominant_min_ratio,
        confirm_large_scan,
        force,
        limits: safety.limits(),
    };
    let prepared = smartzip_engine::PreparedExtractTask::new(
        safety.policy.as_ref().unwrap().as_ref().clone(),
        workflow_request,
    )?;
    let task_id = prepared.identity().task_id.clone();
    let execution = if safety.policy.as_ref().unwrap().values().state.mode
        == smartzip_config::StateMode::ReadWrite
        && safety.policy.as_ref().unwrap().values().state.history
    {
        db.and_then(SmartZipDb::db_path)
            .map(|path| {
                let store = Arc::new(smartzip_engine::state_store::StateStore::start(path)?);
                Ok::<_, Box<dyn std::error::Error>>(Arc::new(
                    smartzip_engine::execution_runtime::ExecutionCoordinator::for_host(store)?,
                ))
            })
            .transpose()?
    } else {
        None
    };
    let stdin_lock = StdinLock::configured(cancellation.clone(), safety, json);
    let password_prompter = StdinPrompter {
        lock: stdin_lock.clone(),
    };
    let output_prompter = StdinOutputPrompter {
        lock: stdin_lock.clone(),
    };
    let embedded_prompter = StdinEmbeddedPrompter {
        lock: stdin_lock.clone(),
    };
    let encoding_prompter = StdinEncodingPrompter {
        lock: stdin_lock.clone(),
    };
    let engine = SmartZipEngine::default().with_cancellation_token(cancellation.clone());
    let event_listener = task_listener(json, verbose_routing, safety);

    if let Some(execution) = &execution {
        execution.submit(prepared.submission(0)?).await?;
    }

    let recovery_work = async {
        let Some(execution) = &execution else {
            return;
        };
        let futures = execution
            .take_runnable_recovery()
            .into_iter()
            .map(|recovered| {
                let execution = execution.as_ref();
                let cancellation = cancellation.clone();
                let event_listener = event_listener.clone();
                let stdin_lock = &stdin_lock;
                async move {
                    let recovered_task_id =
                        smartzip_core::TaskId::from_stored(recovered.task_id.clone());
                    let recovered_result: Result<(), Box<dyn std::error::Error>> = async {
                        let prepared = smartzip_engine::PreparedExtractTask::recover(&recovered)?;
                        let recovered_backend = smartzip_archive::BackendRouter::from_config(
                            &prepared.policy().values().backends,
                        )?;
                        let lock = stdin_lock.for_policy(prepared.policy(), json);
                        let password = StdinPrompter { lock: lock.clone() };
                        let output = StdinOutputPrompter { lock: lock.clone() };
                        let embedded = StdinEmbeddedPrompter { lock: lock.clone() };
                        let encoding = StdinEncodingPrompter { lock: lock.clone() };
                        prepared
                            .run(
                                SmartZipEngine::default()
                                    .with_cancellation_token(cancellation.clone()),
                                &recovered_backend,
                                db.map(SmartZipDb::connection),
                                smartzip_engine::ExtractInteraction {
                                    password: lock
                                        .interactive
                                        .then_some(&password as &dyn InteractivePasswordPrompter),
                                    output: Some(&output),
                                    embedded: Some(&embedded),
                                    encoding: Some(&encoding),
                                },
                                smartzip_engine::ExtractObserver {
                                    listener: event_listener.clone(),
                                    history: None,
                                    execution: Some(execution),
                                },
                            )
                            .await?;
                        Ok(())
                    }
                    .await;
                    match recovered_result {
                        Ok(()) => execution.release_task(&recovered_task_id),
                        Err(error) => {
                            let cancelled = cancellation.is_cancelled();
                            let stop_result = execution
                                .stop_task(
                                    &recovered_task_id,
                                    if cancelled { "cancelled" } else { "failed" },
                                    if cancelled {
                                        "cancelled"
                                    } else {
                                        "recovery_failed"
                                    },
                                )
                                .await;
                            if let Some(listener) = &event_listener {
                                let detail = match stop_result {
                                    Ok(()) => error.to_string(),
                                    Err(stop_error) => format!("{error}; {stop_error}"),
                                };
                                listener(&smartzip_core::TaskEvent {
                                    task_id: recovered_task_id,
                                    kind: smartzip_core::TaskEventKind::Warning {
                                        message: format!("task recovery failed: {detail}"),
                                    },
                                });
                            }
                        }
                    }
                }
            });
        futures_util::future::join_all(futures).await;
    };
    let current_work = prepared.run(
        engine,
        backend,
        db.map(SmartZipDb::connection),
        smartzip_engine::ExtractInteraction {
            password: if stdin_lock.interactive {
                Some(&password_prompter)
            } else {
                None
            },
            output: Some(&output_prompter),
            embedded: Some(&embedded_prompter),
            encoding: Some(&encoding_prompter),
        },
        smartzip_engine::ExtractObserver {
            listener: event_listener.clone(),
            history: None,
            execution: execution.as_ref().map(|execution| {
                execution.as_ref() as &dyn smartzip_engine::ExecutionStateRecorder
            }),
        },
    );
    let (_, result) = tokio::join!(recovery_work, current_work);
    let result = match result {
        Ok(result) => {
            if let Some(execution) = &execution {
                execution.release_task(&task_id);
            }
            result
        }
        Err(error) => {
            if let Some(execution) = &execution {
                let cancelled = matches!(error, smartzip_core::SmartZipError::Cancelled);
                execution
                    .stop_task(
                        &task_id,
                        if cancelled { "cancelled" } else { "failed" },
                        if cancelled {
                            "cancelled"
                        } else {
                            "workflow_failed"
                        },
                    )
                    .await?;
            }
            return Err(error.into());
        }
    };

    let exit_code = result.status.exit_code() as i32;
    print_extract_result(
        &result,
        json,
        db.is_some() && safety.policy.as_ref().unwrap().reads_state(),
    )?;
    command_exit(exit_code)
}

pub(super) fn scanner_config(deep: bool, max_scan_bytes: Option<u64>) -> ScannerConfig {
    let mut config = ScannerConfig {
        mode: if deep { ScanMode::Deep } else { ScanMode::Fast },
        ..ScannerConfig::default()
    };
    config.max_scan_bytes = match max_scan_bytes {
        Some(0) => None,
        Some(value) => Some(value),
        None if deep => None,
        None => config.max_scan_bytes,
    };
    config
}

pub(super) fn parse_encoding_mode(encoding: &str) -> EncodingMode {
    if encoding.eq_ignore_ascii_case("auto") || encoding.eq_ignore_ascii_case("backend") {
        EncodingMode::Auto
    } else {
        EncodingMode::Override(encoding.to_string())
    }
}

pub(super) async fn select_list_encoding(
    path: PathBuf,
    encoding: &str,
    pick_encoding: bool,
    control: StdinLock,
) -> Result<EncodingMode, Box<dyn std::error::Error>> {
    if pick_encoding && !control.interactive {
        return Err(
            "--pick-encoding requires an interactive terminal; use --encoding for scripts".into(),
        );
    }
    if !pick_encoding {
        return Ok(parse_encoding_mode(encoding));
    }
    let choice = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let _guard = control.gate.lock().unwrap();
        let candidates = encoding_preview_candidates();
        eprintln!("\n  Candidate encodings for {}:", path.display());
        for (idx, candidate) in candidates.iter().enumerate() {
            eprintln!("  [{}] {}", idx + 1, candidate);
        }
        eprint!("  Pick encoding number (Enter for auto): ");
        let _ = std::io::stderr().flush();
        terminal_line(&control, false)
    })
    .await?;
    let Some(choice) = choice else {
        return Err(smartzip_core::SmartZipError::Cancelled.into());
    };
    let trimmed = choice.trim();
    if trimmed.is_empty() {
        return Ok(EncodingMode::Auto);
    }
    let idx: usize = trimmed.parse()?;
    let selected = encoding_preview_candidates()
        .get(idx.checked_sub(1).ok_or("invalid encoding choice")?)
        .copied()
        .ok_or_else(|| format!("invalid encoding choice: {trimmed}"))?;
    Ok(parse_encoding_mode(selected))
}

pub(super) fn default_output_dir(first_path: &Path) -> PathBuf {
    first_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}
