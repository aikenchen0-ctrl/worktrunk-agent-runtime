//! 集成清单、实际解析证据与历史构建收据；所有机器状态保存在仓库外。
use super::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const POLICY: &str = "resolved-combination-jvm-v3-scoped-consumers";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    events: Vec<String>,
    project: String,
    configuration: String,
    test_task: String,
    #[serde(default)]
    build_task: Option<String>,
}

fn digest(value: &impl Serialize) -> Result<String> {
    Ok(Sha256::digest(serde_json::to_vec(value)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn read_optional(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(read_json(path)?))
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || b"_-".contains(&ch))
}

impl Manifest {
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.events.is_empty() {
            bail!("集成清单必须使用版本 1 并指定非空 events");
        }
        if !(self.project == ":"
            || (self.project.starts_with(':') && self.project[1..].split(':').all(identifier)))
        {
            bail!("集成 project 必须是完整 Gradle 项目路径");
        }
        let prefix = if self.project == ":" {
            ":".to_string()
        } else {
            format!("{}:", self.project)
        };
        for task in std::iter::once(&self.test_task).chain(self.build_task.iter()) {
            if !task.strip_prefix(&prefix).is_some_and(identifier) {
                bail!("集成任务必须属于清单 project：{}", task);
            }
        }
        if !identifier(&self.configuration) || self.build_task.as_ref() == Some(&self.test_task) {
            bail!("集成 configuration 无效或构建任务与测试任务重复");
        }
        Ok(())
    }
}

struct Inputs {
    manifest: Manifest,
    snapshot: Value,
    sha256: String,
    pending: Vec<String>,
}

impl Runtime {
    fn integration_inputs(&self, path: &Path) -> Result<Inputs> {
        let mut manifest: Manifest =
            read_json(path).context("集成清单格式不完整；请按新版示例指定项目、配置和测试任务")?;
        manifest.validate()?;
        manifest.events.sort();
        let mut ids = BTreeSet::new();
        let mut providers = BTreeSet::new();
        let mut coordinates = BTreeSet::new();
        let mut events = Vec::new();
        let mut pending = Vec::new();
        for id in &manifest.events {
            if !valid_event_id(id) || !ids.insert(id) {
                bail!("集成事件 ID 无效或重复：{}", id);
            }
            let event: Value =
                read_json(&self.state_root.join("events").join(format!("{id}.json")))?;
            if event["event_id"] != *id || event["event_type"] != "contract_candidate" {
                bail!("集成事件身份不匹配：{}", id);
            }
            let provider = event["provider"]
                .as_str()
                .filter(|v| valid_agent_id(v))
                .ok_or_else(|| anyhow::anyhow!("集成事件缺少有效 provider"))?;
            let provider_repository = event["repository"].as_str().unwrap_or_default();
            if !providers.insert((provider_repository.to_string(), provider.to_string())) {
                bail!("同一仓库的提供方不能选择多个候选：{}", provider);
            }
            routing::validate_event_routes(&event)?;
            let artifact = &event["artifact"];
            let coordinate = artifact["coordinate"].as_str().unwrap_or_default();
            let parts: Vec<_> = coordinate.split(':').collect();
            let sha = artifact["sha256"].as_str().unwrap_or_default();
            let url = artifact["url"].as_str().unwrap_or_default();
            if parts.len() != 3
                || parts.iter().any(|s| s.is_empty())
                || event["candidate_version"] != parts[2]
                || coordinate.contains(['+', '[', ']', '(', ')'])
                || parts[2].contains("SNAPSHOT")
                || parts[2].starts_with("latest.")
                || sha.len() != 64
                || !sha.bytes().all(|ch| ch.is_ascii_hexdigit())
                || !(url.starts_with("https://") || url.starts_with("http://"))
            {
                bail!("事件 {} 的固定坐标、摘要或 URL 无效", id);
            }
            if !coordinates.insert(format!("{}:{}", parts[0], parts[1])) {
                bail!("多个事件重复指定同一 Maven 模块");
            }
            let extension = url
                .split(['?', '#'])
                .next()
                .unwrap_or_default()
                .rsplit('.')
                .next()
                .unwrap_or_default();
            if !matches!(extension, "aar" | "jar") {
                bail!("集成候选只支持 AAR/JAR");
            }
            let consumers = event["affected_consumers"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("事件缺少消费者数组"))?;
            let mut seen = BTreeSet::new();
            let mut acknowledgements = serde_json::Map::new();
            for value in consumers {
                let consumer = value
                    .as_str()
                    .filter(|v| valid_agent_id(v))
                    .ok_or_else(|| anyhow::anyhow!("消费者名称无效"))?;
                if !seen.insert(consumer) {
                    bail!("消费者重复：{}", consumer);
                }
                let ack = read_optional(
                    &self
                        .state_root
                        .join("events")
                        .join(id)
                        .join("acks")
                        .join(format!("{consumer}.json")),
                )?;
                if !ack
                    .as_ref()
                    .is_some_and(|ack| passed_ack(ack, &event, consumer))
                {
                    pending.push(format!("{id}/{consumer}"));
                }
                acknowledgements.insert(consumer.into(), ack.unwrap_or(Value::Null));
            }
            events.push(json!({"event":event, "extension":extension, "acks":acknowledgements}));
        }
        let snapshot = json!({"policy_version":POLICY, "verifier_sha256":digest(&include_str!("../scripts/verify-integration.gradle"))?, "manifest":manifest, "events":events});
        let sha256 = digest(&snapshot)?;
        Ok(Inputs {
            manifest,
            snapshot,
            sha256,
            pending,
        })
    }

    fn combination_index(&self, metadata: &Metadata, sha: &str) -> PathBuf {
        self.state_root
            .join("integrations")
            .join(&metadata.worktree_id)
            .join(format!("{sha}.json"))
    }

    pub fn run_integration(
        &self,
        manifest_path: &str,
        args: &[String],
        json_output: bool,
        allow_pending_consumers: bool,
    ) -> Result<()> {
        validate_extra_arguments(args)?;
        let metadata = self.ensure_metadata(false)?;
        if metadata.is_primary {
            bail!("集成验证必须使用非主 Worktree");
        }
        require_clean_worktree(&self.cwd)?;
        let manifest_path = fs::canonicalize(manifest_path)?;
        let inputs = self.integration_inputs(&manifest_path)?;
        check_consumer_gate(&inputs, allow_pending_consumers)?;
        let run_id = unique_token();
        let run_dir = PathBuf::from(&metadata.paths.root)
            .join("integration-receipts")
            .join(&run_id);
        fs::create_dir_all(&run_dir)?;
        let request = json!({"integration_id":run_id, "input_sha256":inputs.sha256, "inputs":inputs.snapshot,
            "evidence_path": normalized_process_path(&run_dir.join("evidence.json"))});
        write_json_atomic(&run_dir.join("inputs.json"), &request)?;
        let encoded = base64(&serde_json::to_vec(&request)?);
        let script = format!(
            "def integrationRequest = new groovy.json.JsonSlurper().parseText(new String('{encoded}'.decodeBase64(), 'UTF-8'))\n{}",
            include_str!("../scripts/verify-integration.gradle")
        );
        let script_path = run_dir.join("verify.gradle");
        write_text_atomic(&script_path, &script)?;
        let wrapper = self.cwd.join(if cfg!(windows) {
            "gradlew.bat"
        } else {
            "gradlew"
        });
        if !wrapper.is_file() {
            bail!("集成 Worktree 缺少 Gradle Wrapper");
        }
        let mut tasks = vec![
            format!("-I{}", normalized_process_path(&script_path).display()),
            "--no-configuration-cache".into(),
        ];
        if let Some(task) = &inputs.manifest.build_task {
            tasks.push(task.clone());
        }
        tasks.push(inputs.manifest.test_task.clone());
        tasks.extend_from_slice(args);
        let workers = env::var("AGENT_MAX_WORKERS").ok();
        if workers
            .as_ref()
            .is_some_and(|v| v.parse::<u32>().ok().is_none_or(|v| v == 0))
        {
            bail!("AGENT_MAX_WORKERS 必须是正整数");
        }
        let token = acquire_lock_internal(self, 0, 7200, None)?;
        // 锁持有至收据归档结束，避免下一次构建替换 build-result.json。
        let outcome = (|| -> Result<Value> {
            write_json_atomic(
                &self.combination_index(&metadata, &inputs.sha256),
                &json!({
                    "status":"running", "integration_id":run_id, "owner":metadata.agent_id,
                    "pid":std::process::id(), "started_at":Utc::now().to_rfc3339(), "receipt":run_dir.join("receipt.json")
                }),
            )?;
            if self.integration_inputs(&manifest_path)?.sha256 != inputs.sha256 {
                bail!("验证输入在启动前变化");
            }
            let build = self.run_gradle_locked(
                &metadata,
                &wrapper,
                &tasks,
                workers.as_deref(),
                Some(&run_id),
                json_output,
            );
            let current: Option<BuildReceipt> = read_json::<BuildReceipt>(
                &PathBuf::from(&metadata.paths.root).join("build-result.json"),
            )
            .ok()
            .filter(|r| r.event_id.as_deref() == Some(run_id.as_str()));
            let evidence = read_optional(&run_dir.join("evidence.json"))?;
            let validation = (|| -> Result<()> {
                build?;
                let current = current
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("缺少本次构建收据"))?;
                validate_build(current, &metadata, &run_id)?;
                require_clean_worktree(&self.cwd)?;
                validate_evidence(
                    evidence
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("缺少本次组合制品和测试证据"))?,
                    &inputs,
                    &run_id,
                )?;
                if self.integration_inputs(&manifest_path)?.sha256 != inputs.sha256 {
                    bail!("清单、事件或消费者证据在构建期间变化");
                }
                Ok(())
            })();
            let error = validation.err().map(|error| format!("{error:#}"));
            let receipt = json!({"schema_version":2, "integration_id":run_id, "input_sha256":inputs.sha256,
                "policy_version":POLICY, "manifest_path":manifest_path, "metadata":metadata,
                "allow_pending_consumers":allow_pending_consumers, "pending_consumers":inputs.pending,
                "build_receipt":current, "evidence":evidence, "success":error.is_none(),
                "ready_for_integration":error.is_none() && inputs.pending.is_empty(), "release_approved":false,
                "error":error, "finished_at":Utc::now().to_rfc3339()});
            write_json_atomic(&run_dir.join("receipt.json"), &receipt)?;
            write_json_atomic(
                &self.combination_index(&metadata, &inputs.sha256),
                &json!({"status":"finished", "receipt":run_dir.join("receipt.json"), "integration_id":run_id}),
            )?;
            Ok(receipt)
        })();
        let release = self.release_build_lock(Some(&token), true);
        release?;
        let receipt = outcome?;
        if json_output {
            println!("{}", serde_json::to_string_pretty(&receipt)?);
        } else {
            println!(
                "组合验证结果：{}；证据：{}",
                if receipt["success"] == true {
                    "通过"
                } else {
                    "失败"
                },
                run_dir.display()
            );
        }
        if receipt["success"] != true {
            bail!("组合验证失败：{}", receipt["error"]);
        }
        if !json_output && !inputs.pending.is_empty() {
            println!("候选组合测试通过，但仍有消费者未确认；集成未就绪，不允许发布");
        }
        Ok(())
    }

    pub fn integration_status(&self, manifest_path: &str, json_output: bool) -> Result<()> {
        let report = self.integration_report(manifest_path)?;
        if json_output {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!(
                "组合测试：{}；集成就绪：{}；{}",
                if report["combination_verified"] == true {
                    "通过"
                } else {
                    "未通过"
                },
                if report["ready_for_integration"] == true {
                    "是"
                } else {
                    "否"
                },
                report["reason"]
            );
        }
        if report["ready_for_integration"] != true {
            bail!("组合尚未满足集成条件");
        }
        Ok(())
    }

    fn integration_report(&self, manifest_path: &str) -> Result<Value> {
        let inputs = self.integration_inputs(Path::new(manifest_path))?;
        let metadata = self.ensure_metadata(false)?;
        let index = read_optional(&self.combination_index(&metadata, &inputs.sha256))?;
        let receipt = index
            .as_ref()
            .and_then(|v| v["receipt"].as_str())
            .map(|path| read_optional(Path::new(path)))
            .transpose()?
            .flatten();
        let verified = (|| -> Result<()> {
            require_clean_worktree(&self.cwd)?;
            if index.as_ref().is_none_or(|v| v["status"] != "finished") {
                bail!("组合验证尚未完成或运行被中断");
            }
            let receipt = receipt
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("缺少当前清单和消费者证据对应的组合验证"))?;
            if receipt["schema_version"] != 2
                || receipt["success"] != true
                || receipt["input_sha256"] != inputs.sha256
                || receipt["policy_version"] != POLICY
            {
                bail!("组合收据失败或已失效");
            }
            let build: BuildReceipt = serde_json::from_value(receipt["build_receipt"].clone())?;
            let run_id = receipt["integration_id"].as_str().unwrap_or_default();
            validate_build(&build, &metadata, run_id)?;
            validate_evidence(&receipt["evidence"], &inputs, run_id)
        })();
        let combination_verified = verified.is_ok();
        let ready = combination_verified && inputs.pending.is_empty();
        let reason = verified.err().map(|e| format!("{e:#}")).or_else(|| {
            (!inputs.pending.is_empty()).then(|| "组合测试通过，但仍有消费者未验证".to_string())
        });
        Ok(
            json!({"input_sha256":inputs.sha256, "pending_consumers":inputs.pending,
            "ready_for_integration":ready, "combination_verified":combination_verified, "release_approved":false,
            "reason":reason, "receipt":receipt}),
        )
    }
}

fn check_consumer_gate(inputs: &Inputs, allow_pending_consumers: bool) -> Result<()> {
    // 探索验证用于联合迁移；它只放开测试入口，不产生消费者确认或发布许可。
    if !allow_pending_consumers && !inputs.pending.is_empty() {
        bail!(
            "消费者尚未验证：{}；联合迁移可显式使用 --allow-pending-consumers 先测试候选组合",
            inputs.pending.join(", ")
        );
    }
    Ok(())
}

fn validate_extra_arguments(args: &[String]) -> Result<()> {
    for arg in args {
        let property = arg
            .strip_prefix("-P")
            .and_then(|v| v.split_once('='))
            .is_some_and(|(key, _)| identifier(key));
        if !property
            && !matches!(
                arg.as_str(),
                "--offline" | "--stacktrace" | "--rerun-tasks" | "--info"
            )
        {
            bail!(
                "集成任务由清单指定；附加参数仅支持 -P名称=值、--offline、--stacktrace、--rerun-tasks、--info：{}",
                arg
            );
        }
    }
    Ok(())
}

fn validate_build(build: &BuildReceipt, metadata: &Metadata, run_id: &str) -> Result<()> {
    if run_id.is_empty()
        || build.event_id.as_deref() != Some(run_id)
        || !build.success
        || build.exit_code != Some(0)
        || !build.worktree_clean
        || build.worktree_id != metadata.worktree_id
        || build.runtime_id != metadata.runtime_id
        || build.worktree_path != metadata.worktree_path
        || build.commit != metadata.commit
        || build.repository != metadata.repository
    {
        bail!("组合构建收据未绑定当前提交和本次成功运行");
    }
    Ok(())
}

pub(super) fn passed_ack(ack: &Value, event: &Value, consumer: &str) -> bool {
    if verified_ack_status(ack) != "passed"
        || ack["event_id"] != event["event_id"]
        || ack["consumer"] != consumer
    {
        return false;
    }
    let Ok(receipt) = serde_json::from_value::<BuildReceipt>(ack["receipt_snapshot"].clone())
    else {
        return false;
    };
    let metadata = Metadata {
        schema_version: 1,
        runtime_id: receipt.runtime_id.clone(),
        agent_id: receipt.agent_id.clone(),
        worktree_id: receipt.worktree_id.clone(),
        repository: receipt.repository.clone(),
        worktree_path: receipt.worktree_path.clone(),
        commit: receipt.commit.clone(),
        branch: None,
        is_primary: false,
        created_at: String::new(),
        last_seen_at: String::new(),
        status: String::new(),
        paths: Paths {
            root: String::new(),
            worktree_root: String::new(),
            gradle_user_home: String::new(),
            maven_local: String::new(),
            temp: String::new(),
            logs: String::new(),
        },
    };
    ack["agent_id"] == receipt.agent_id
        && ack["worktree_id"] == receipt.worktree_id
        && validate_candidate_receipt(&receipt, &metadata, event, ack, consumer).is_ok()
}

fn validate_evidence(evidence: &Value, inputs: &Inputs, run_id: &str) -> Result<()> {
    if evidence["schema_version"] != 1
        || evidence["integration_id"] != run_id
        || evidence["input_sha256"] != inputs.sha256
        || evidence["project"] != inputs.manifest.project
        || evidence["configuration"] != inputs.manifest.configuration
        || evidence["test_task"] != inputs.manifest.test_task
        || evidence["test_executed"] != true
        || evidence["tests"].as_u64().unwrap_or(0)
            <= evidence["skipped"].as_u64().unwrap_or(u64::MAX)
        || evidence["failures"].as_u64() != Some(0)
        || evidence["graph_sha256"]
            .as_str()
            .is_none_or(|v| v.len() != 64)
    {
        bail!("组合制品解析或实际测试证据不完整");
    }
    if let Some(task) = &inputs.manifest.build_task {
        if evidence["build_task"] != *task || evidence["build_executed"] != true {
            bail!("集成构建任务未执行");
        }
    }
    let artifacts = evidence["candidates"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("组合候选证据缺失"))?;
    let events = inputs.snapshot["events"]
        .as_array()
        .expect("已校验事件数组");
    if artifacts.len() != events.len() {
        bail!("实际组合候选数量不匹配");
    }
    for event in events {
        let expected = &event["event"]["artifact"];
        let matches: Vec<_> = artifacts
            .iter()
            .filter(|a| {
                a["coordinate"] == expected["coordinate"]
                    && a["sha256"]
                        .as_str()
                        .zip(expected["sha256"].as_str())
                        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
            })
            .collect();
        if matches.len() != 1 {
            bail!("组合未唯一使用指定候选：{}", expected["coordinate"]);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Runtime, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let runtime = Runtime::with_paths(temp.path().into(), temp.path().join("state"));
        let path = temp.path().join("manifest.json");
        write_json_atomic(&path, &json!({"schema_version":1, "events":["a","b"], "project":":shell", "configuration":"debugRuntimeClasspath", "test_task":":shell:testDebugUnitTest"})).unwrap();
        for id in ["a", "b"] {
            write_json_atomic(&runtime.state_root.join("events").join(format!("{id}.json")), &json!({
                "event_id":id, "event_type":"contract_candidate", "provider":id, "candidate_version":"1.0.0-dev.abc",
                "affected_consumers":[], "artifact":{"coordinate":format!("example:{id}:1.0.0-dev.abc"), "sha256":"a".repeat(64), "url":format!("https://example.invalid/{id}.aar")}
            })).unwrap();
        }
        (temp, runtime, path)
    }

    fn evidence(inputs: &Inputs) -> Value {
        json!({"schema_version":1, "integration_id":"run", "input_sha256":inputs.sha256,
            "project":inputs.manifest.project, "configuration":inputs.manifest.configuration,
            "test_task":inputs.manifest.test_task, "test_executed":true, "tests":2, "failures":0, "skipped":0,
            "graph_sha256":"b".repeat(64), "candidates":inputs.snapshot["events"].as_array().unwrap().iter()
                .map(|v|json!({"coordinate":v["event"]["artifact"]["coordinate"], "sha256":v["event"]["artifact"]["sha256"]})).collect::<Vec<_>>()})
    }

    #[test]
    fn combination_requires_every_resolved_candidate_and_new_test_execution() {
        let (_temp, runtime, path) = setup();
        let inputs = runtime.integration_inputs(&path).unwrap();
        let good = evidence(&inputs);
        assert!(validate_evidence(&good, &inputs, "run").is_ok());
        for (key, value) in [
            ("integration_id", json!("old")),
            ("input_sha256", json!("old")),
            ("test_executed", json!(false)),
            ("tests", json!(0)),
            ("skipped", json!(2)),
            ("failures", json!(1)),
            ("configuration", json!("other")),
        ] {
            let mut bad = good.clone();
            bad[key] = value;
            assert!(validate_evidence(&bad, &inputs, "run").is_err(), "{key}");
        }
        let mut bad = good.clone();
        bad["candidates"].as_array_mut().unwrap().pop();
        assert!(validate_evidence(&bad, &inputs, "run").is_err());
        let mut bad = good.clone();
        bad["candidates"][0]["sha256"] = json!("c".repeat(64));
        assert!(validate_evidence(&bad, &inputs, "run").is_err());
        let mut bad = good.clone();
        bad["candidates"][0] = bad["candidates"][1].clone();
        assert!(validate_evidence(&bad, &inputs, "run").is_err());
    }

    #[test]
    fn optional_build_task_must_have_execution_evidence() {
        let (_temp, runtime, path) = setup();
        let mut inputs = runtime.integration_inputs(&path).unwrap();
        inputs.manifest.build_task = Some(":shell:assembleDebug".into());
        let mut value = evidence(&inputs);
        assert!(validate_evidence(&value, &inputs, "run").is_err());
        value["build_task"] = json!(":shell:assembleDebug");
        value["build_executed"] = json!(true);
        assert!(validate_evidence(&value, &inputs, "run").is_ok());
    }

    #[test]
    fn input_identity_includes_event_content_and_acknowledgements() {
        let (_temp, runtime, path) = setup();
        let original = runtime.integration_inputs(&path).unwrap();
        let event_path = runtime.state_root.join("events/a.json");
        let mut event: Value = read_json(&event_path).unwrap();
        event["artifact"]["sha256"] = json!("b".repeat(64));
        write_json_atomic(&event_path, &event).unwrap();
        assert_ne!(
            original.sha256,
            runtime.integration_inputs(&path).unwrap().sha256
        );
        event["affected_consumers"] = json!(["consumer"]);
        write_json_atomic(&event_path, &event).unwrap();
        let pending = runtime.integration_inputs(&path).unwrap();
        assert_eq!(pending.pending, vec!["a/consumer"]);
        write_json_atomic(
            &runtime.state_root.join("events/a/acks/consumer.json"),
            &json!({"status":"passed","evidence_version":2,"receipt_snapshot":{"validation":{}}}),
        )
        .unwrap();
        let invalid = runtime.integration_inputs(&path).unwrap();
        assert_eq!(invalid.pending, pending.pending);
        assert_ne!(invalid.sha256, pending.sha256);
    }

    #[test]
    fn pending_consumers_require_explicit_exploratory_validation() {
        let (_temp, runtime, path) = setup();
        let mut inputs = runtime.integration_inputs(&path).unwrap();
        assert!(check_consumer_gate(&inputs, false).is_ok());
        inputs.pending = vec!["a/consumer".into()];
        assert!(check_consumer_gate(&inputs, false).is_err());
        assert!(check_consumer_gate(&inputs, true).is_ok());
        assert_eq!(inputs.pending, vec!["a/consumer"]);
    }

    #[test]
    fn manifest_order_does_not_change_identity_and_unknown_fields_are_rejected() {
        let (_temp, runtime, path) = setup();
        let original = runtime.integration_inputs(&path).unwrap().sha256;
        let mut manifest: Value = read_json(&path).unwrap();
        manifest["events"] = json!(["b", "a"]);
        write_json_atomic(&path, &manifest).unwrap();
        assert_eq!(original, runtime.integration_inputs(&path).unwrap().sha256);
        manifest["integration_receipt"] = json!("old-success.json");
        write_json_atomic(&path, &manifest).unwrap();
        assert!(runtime.integration_inputs(&path).is_err());
    }

    #[test]
    fn event_validation_rejects_missing_consumers_and_conflicting_coordinates() {
        let (_temp, runtime, path) = setup();
        let event_path = runtime.state_root.join("events/b.json");
        let initial: Value = read_json(&event_path).unwrap();
        let mut bad = initial.clone();
        bad.as_object_mut().unwrap().remove("affected_consumers");
        write_json_atomic(&event_path, &bad).unwrap();
        assert!(runtime.integration_inputs(&path).is_err());
        let mut bad = initial.clone();
        bad["artifact"]["coordinate"] = json!("example:a:1.0.0-dev.abc");
        write_json_atomic(&event_path, &bad).unwrap();
        assert!(runtime.integration_inputs(&path).is_err());
        let mut bad = initial.clone();
        bad["event_id"] = json!("different");
        write_json_atomic(&event_path, &bad).unwrap();
        assert!(runtime.integration_inputs(&path).is_err());
        let mut bad = initial.clone();
        bad["affected_consumers"] = json!(["c", "c"]);
        write_json_atomic(&event_path, &bad).unwrap();
        assert!(runtime.integration_inputs(&path).is_err());
    }

    #[test]
    fn status_rejects_missing_interrupted_failed_dirty_and_stale_receipts() {
        let (temp, mut runtime, path) = setup();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let git = |cwd: &Path, args: &[&str]| {
            let result = Command::new("git")
                .current_dir(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        };
        git(&repo, &["init", "-b", "main"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );
        let linked = temp.path().join("linked");
        git(
            &repo,
            &["worktree", "add", "--detach", linked.to_str().unwrap()],
        );
        runtime.cwd = linked.clone();
        let metadata = runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let inputs = runtime.integration_inputs(&path).unwrap();
        assert!(
            runtime
                .integration_status(path.to_str().unwrap(), true)
                .is_err()
        );
        let receipt_path = temp.path().join("receipt.json");
        let index_path = runtime.combination_index(&metadata, &inputs.sha256);
        let build = json!({"schema_version":1,"runtime_id":metadata.runtime_id,"agent_id":metadata.agent_id,
            "worktree_id":metadata.worktree_id,"worktree_path":metadata.worktree_path,"repository":metadata.repository,
            "commit":metadata.commit,"event_id":"run","command":[inputs.manifest.test_task],"worktree_clean":true,
            "success":true,"exit_code":0,"started_at":Utc::now().to_rfc3339(),"finished_at":Utc::now().to_rfc3339(),
            "elapsed_ms":1,"stdout_log":"test-out","stderr_log":"test-err"});
        let receipt = json!({"schema_version":2,"integration_id":"run","input_sha256":inputs.sha256,
            "policy_version":POLICY,"success":true,"build_receipt":build,"evidence":evidence(&inputs)});
        write_json_atomic(&receipt_path, &receipt).unwrap();
        write_json_atomic(
            &index_path,
            &json!({"status":"finished","receipt":receipt_path}),
        )
        .unwrap();
        assert!(
            runtime
                .integration_status(path.to_str().unwrap(), true)
                .is_ok()
        );
        // 联合迁移测试成功也不能替代其他消费者的真实确认。
        let event_path = runtime.state_root.join("events/a.json");
        let original_event: Value = read_json(&event_path).unwrap();
        let mut pending_event = original_event.clone();
        pending_event["affected_consumers"] = json!(["unmigrated"]);
        write_json_atomic(&event_path, &pending_event).unwrap();
        let pending_inputs = runtime.integration_inputs(&path).unwrap();
        let pending_receipt_path = temp.path().join("pending-receipt.json");
        let mut pending_receipt = receipt.clone();
        pending_receipt["input_sha256"] = json!(pending_inputs.sha256);
        pending_receipt["evidence"] = evidence(&pending_inputs);
        pending_receipt["allow_pending_consumers"] = json!(true);
        write_json_atomic(&pending_receipt_path, &pending_receipt).unwrap();
        write_json_atomic(
            &runtime.combination_index(&metadata, &pending_inputs.sha256),
            &json!({"status":"finished","receipt":pending_receipt_path}),
        )
        .unwrap();
        let report = runtime.integration_report(path.to_str().unwrap()).unwrap();
        assert_eq!(report["combination_verified"], true);
        assert_eq!(report["ready_for_integration"], false);
        assert_eq!(report["release_approved"], false);
        assert_eq!(report["pending_consumers"], json!(["a/unmigrated"]));
        assert!(
            runtime
                .integration_status(path.to_str().unwrap(), true)
                .is_err()
        );
        assert!(
            !runtime
                .state_root
                .join("events/a/acks/unmigrated.json")
                .exists()
        );
        write_json_atomic(&event_path, &original_event).unwrap();
        write_json_atomic(
            &index_path,
            &json!({"status":"running","receipt":receipt_path}),
        )
        .unwrap();
        assert!(
            runtime
                .integration_status(path.to_str().unwrap(), true)
                .is_err()
        );
        write_json_atomic(
            &index_path,
            &json!({"status":"finished","receipt":receipt_path}),
        )
        .unwrap();
        let mut failed = receipt.clone();
        failed["success"] = json!(false);
        write_json_atomic(&receipt_path, &failed).unwrap();
        assert!(
            runtime
                .integration_status(path.to_str().unwrap(), true)
                .is_err()
        );
        write_json_atomic(&receipt_path, &receipt).unwrap();
        fs::write(linked.join("ChangedApi.kt"), "public class ChangedApi").unwrap();
        assert!(
            runtime
                .integration_status(path.to_str().unwrap(), true)
                .is_err()
        );
        git(&linked, &["add", "ChangedApi.kt"]);
        git(
            &linked,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "changed API",
            ],
        );
        assert!(
            runtime
                .integration_status(path.to_str().unwrap(), true)
                .is_err()
        );
    }

    #[test]
    fn gradle_arguments_cannot_skip_verifier_or_substitute_integration_tasks() {
        for args in [
            vec!["help"],
            vec!["-x", ":shell:verifyIntegrationCandidates"],
            vec!["--dry-run"],
            vec!["-Iother.gradle"],
            vec!["--tests=unrelated"],
            vec!["--configuration-cache"],
        ] {
            assert!(
                validate_extra_arguments(&args.into_iter().map(str::to_string).collect::<Vec<_>>())
                    .is_err()
            );
        }
        assert!(
            validate_extra_arguments(&["-PVERSION=1.0.0-dev.abc".into(), "--offline".into()])
                .is_ok()
        );
        assert!(validate_extra_arguments(&["--info".into()]).is_ok());
    }
}
