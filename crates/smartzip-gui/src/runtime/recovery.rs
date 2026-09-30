use super::*;

pub(super) async fn run(
    request: JobRequest,
    mailbox: Mailbox,
    cancellation: CancellationToken,
    roots: Arc<smartzip_engine::root_management::RootManagement>,
) -> Result<JobOutcome, Box<dyn std::error::Error>> {
    let id = request
        .settings
        .recovery_task_id
        .as_deref()
        .ok_or("缺少恢复任务标识")?;
    let policy = load_policy(&request)?;
    if policy.values().state.mode != smartzip_config::StateMode::ReadWrite
        || !policy.values().state.history
    {
        return Err("恢复任务需要启用可写状态与任务历史".into());
    }
    let db = open_database(&policy)?.ok_or("未找到任务数据库")?;
    let execution = shared_state_store(db.db_path().ok_or("恢复任务需要文件数据库")?)?;
    let recovered = execution
        .recovery_snapshot()
        .iter()
        .find(|t| t.task_id == id)
        .ok_or("任务已经结束、正在执行，或不再可恢复")?;
    let (plan, identity) =
        smartzip_engine::execution_runtime::ExecutionCoordinator::decode_recovery(recovered)?;
    for input in &plan.request.inputs {
        if !input.try_exists()? {
            return Err(format!(
                "恢复所需输入不存在：{}。请还原该输入后重试，或从历史记录重新添加原始归档。",
                input.display()
            )
            .into());
        }
    }
    let policy = CompiledRunPolicy::compile(smartzip_config::ResolvedConfig {
        values: plan.policy,
        origins: Default::default(),
        path: None,
        diagnostics: vec![],
    })?;
    let backend = smartzip_archive::BackendRouter::from_config(&policy.values().backends)?;
    let passwords = smartzip_passwords::PasswordService::configured(
        Some(smartzip_db::password::PasswordRepository::new(
            db.connection(),
        )),
        policy.values().passwords.clone(),
        policy.values().state.mode,
    );
    let prompts = Prompter {
        mailbox: mailbox.clone(),
        cancellation: cancellation.clone(),
        gate: Arc::new(tokio::sync::Mutex::new(())),
    };
    let interactive = policy.values().interaction.mode != smartzip_config::InteractionMode::Never;
    let event_mailbox = mailbox.clone();
    let listener: Option<smartzip_engine::TaskEventListener> = Some(Arc::new(move |event| {
        event_mailbox.push(JobMessage::Event(event.clone()))
    }));
    let mut statement=db.connection().prepare("SELECT node_id,root_node_id,input_path,output_path,status,execution_state,stage FROM file_extractions WHERE task_id=?1 AND node_id IS NOT NULL AND parent_node_id IS NULL")?;
    let history = statement
        .query_map([id], |r| {
            let node: String = r.get(0)?;
            let root: Option<String> = r.get(1)?;
            let status: String = r.get(4)?;
            Ok(smartzip_engine::root_management::FileTaskSnapshot {
                node_id: smartzip_core::NodeId::from_stored(node.clone()),
                root_id: smartzip_core::NodeId::from_stored(root.unwrap_or(node)),
                parent_id: None,
                path: PathBuf::from(r.get::<_, String>(2)?),
                output: r.get::<_, Option<String>>(3)?.map(PathBuf::from),
                state: if status == "extracted" {
                    "completed".into()
                } else {
                    r.get(5)?
                },
                stage: r.get(6)?,
                committed: status == "extracted",
                volumes: None,
                events: vec![],
                depth: 0,
                progress: None,
                backend: String::new(),
                root_outcome: None,
                root_activity: None,
                pause_requested: false,
                cancel_requested: false,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    roots.restore_root_history(history);
    // Finish all fallible preflight reads before consuming the claim so they remain retryable.
    execution.set_paused(&identity.task_id, false).await?;
    let _claim = execution
        .take_recovery(id)
        .ok_or("此恢复任务已被另一操作接管")?;
    let engine = SmartZipEngine::default()
        .with_cancellation_token(cancellation.clone())
        .with_root_management(roots)
        .with_run_policy(policy);
    let result = engine
        .extract_task(
            identity.clone(),
            &backend,
            &passwords,
            plan.request,
            smartzip_engine::ExtractInteraction {
                password: interactive.then_some(&prompts as &dyn InteractivePasswordPrompter),
                output: Some(&prompts),
                embedded: Some(&prompts),
                encoding: Some(&prompts),
            },
            smartzip_engine::ExtractObserver {
                listener,
                history: None,
                execution: Some(execution.as_ref()),
            },
        )
        .await;
    match result {
        Ok(result) => {
            execution.release_task(&identity.task_id);
            let status = serde_json::to_value(result.status)?
                .as_str()
                .unwrap_or("failed")
                .to_owned();
            Ok(JobOutcome {
                status,
                detail: serde_json::to_value(result)?,
                warnings: vec!["已按原任务配置恢复；临时密码未保存，源归档不会追补回收。".into()],
            })
        }
        Err(error) => {
            execution
                .stop_task(
                    &identity.task_id,
                    if cancellation.is_cancelled() {
                        "cancelled"
                    } else {
                        "failed"
                    },
                    "recovery_failed",
                )
                .await?;
            Err(error.into())
        }
    }
}
