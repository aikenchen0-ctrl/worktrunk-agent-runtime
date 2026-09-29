//! agentctl 的命令入口。
//!
//! 该程序只管理 Worktree 外部的本机运行时资源，不修改 Worktrunk 或业务仓库源码。

mod cli;
mod runtime;

use anyhow::{Result, bail};
use runtime::{ArtifactEvidence, ContractEventOptions, PrepareOptions, Runtime};
use std::env;

fn main() -> Result<()> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        print_help();
        return Ok(());
    }
    if args[0] == "--version" || args[0] == "-V" {
        println!("agentctl {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let command = args.remove(0);
    // 在发现仓库或创建状态之前拒绝歧义输入，避免报错时操作已经生效。
    cli::validate(&command, &args)?;
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        print_help();
        return Ok(());
    }
    let lifecycle = take_flag_before_separator(&mut args, "--lifecycle")?;
    let worktree_path = if command == "cleanup" {
        take_value_before_separator(&mut args, "--worktree-path")?
    } else {
        None
    };
    let worktree_branch = if command == "cleanup" {
        take_value_before_separator(&mut args, "--worktree-branch")?
    } else {
        None
    };
    let worktree_scope = command == "cleanup"
        && (args.iter().any(|arg| arg == "--worktree")
            || worktree_path.is_some()
            || worktree_branch.is_some());
    let mut forwarded = false;
    let mut agent_id = if lifecycle {
        Some(Runtime::lifecycle_agent_id(&env::current_dir()?)?)
    } else if worktree_scope {
        None
    } else {
        take_value_before_separator(&mut args, "--agent-id")?
    };
    if command == "prepare" && agent_id.is_none() {
        agent_id = take_value(&mut args, "--agent-id")?;
    }
    let runtime = if worktree_scope {
        Runtime::discover_for_worktree()?
    } else {
        Runtime::discover_for_agent(agent_id.as_deref())?
    };
    match command.as_str() {
        "prepare" => println!(
            "{}",
            serde_json::to_string_pretty(&runtime.prepare(PrepareOptions {
                allow_primary: take_flag(&mut args, "--allow-primary")?,
            })?)?
        ),
        "lifecycle-id" => println!("{}", Runtime::lifecycle_agent_id(&env::current_dir()?)?),
        "env" => runtime.print_env(
            &take_value(&mut args, "--format")?.unwrap_or_else(|| "shell".to_string()),
        )?,
        "candidate-version" => {
            let base = take_value(&mut args, "--base")?
                .ok_or_else(|| anyhow::anyhow!("candidate-version 必须指定 --base"))?;
            println!("{}", runtime.candidate_version(&base)?);
        }
        "dependency-audit" => {
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("dependency-audit --format 只支持 text 或 json");
            }
            let enforce = take_flag(&mut args, "--enforce")?;
            runtime.dependency_audit(format == "json", enforce)?;
        }
        "dependency-graph" => {
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("dependency-graph --format 只支持 text 或 json");
            }
            runtime.dependency_graph(format == "json")?;
        }
        "affected-consumers" => {
            let provider = take_value(&mut args, "--provider")?
                .ok_or_else(|| anyhow::anyhow!("affected-consumers 必须指定 --provider 模块名"))?;
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("affected-consumers --format 只支持 text 或 json");
            }
            runtime.affected_consumers(&provider, format == "json")?;
        }
        "contract-snapshot" => {
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("contract-snapshot --format 只支持 text 或 json");
            }
            let output = take_value(&mut args, "--output")?;
            let module = take_value(&mut args, "--module")?;
            runtime.contract_snapshot(format == "json", output.as_deref(), module.as_deref())?;
        }
        "contract-diff" => {
            let baseline = take_value(&mut args, "--baseline")?
                .ok_or_else(|| anyhow::anyhow!("contract-diff 必须指定 --baseline 文件"))?;
            let module = take_value(&mut args, "--module")?;
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("contract-diff --format 只支持 text 或 json");
            }
            let enforce = take_flag(&mut args, "--enforce")?;
            runtime.contract_diff(&baseline, module.as_deref(), format == "json", enforce)?;
        }
        "contract-event" => {
            let provider = take_value(&mut args, "--provider")?
                .ok_or_else(|| anyhow::anyhow!("contract-event 必须指定 --provider 模块名"))?;
            let base = take_value(&mut args, "--base")?
                .ok_or_else(|| anyhow::anyhow!("contract-event 必须指定 --base 版本"))?;
            let baseline = take_value(&mut args, "--baseline")?;
            let module = take_value(&mut args, "--module")?;
            let artifact_url = take_value(&mut args, "--artifact-url")?;
            let artifact_sha256 = take_value(&mut args, "--artifact-sha256")?;
            let artifact_coordinate = take_value(&mut args, "--artifact-coordinate")?;
            let consumer_targets = take_value(&mut args, "--consumer-targets")?;
            let artifact = (artifact_url.is_some()
                || artifact_sha256.is_some()
                || artifact_coordinate.is_some())
            .then_some(ArtifactEvidence {
                url: artifact_url,
                sha256: artifact_sha256,
                coordinate: artifact_coordinate,
            });
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("contract-event --format 只支持 text 或 json");
            }
            runtime.create_contract_event(ContractEventOptions {
                provider: &provider,
                base: &base,
                baseline: baseline.as_deref(),
                module: module.as_deref(),
                artifact: artifact.as_ref(),
                consumer_targets: consumer_targets.as_deref(),
                json: format == "json",
            })?;
        }
        "event-inbox" => {
            let consumer = take_value(&mut args, "--consumer")?
                .ok_or_else(|| anyhow::anyhow!("event-inbox 必须指定 --consumer 模块名"))?;
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("event-inbox --format 只支持 text 或 json");
            }
            runtime.event_inbox(&consumer, format == "json")?;
        }
        "event-import" => {
            let file = take_value(&mut args, "--file")?
                .ok_or_else(|| anyhow::anyhow!("event-import 必须指定 --file 事件文件"))?;
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("event-import --format 只支持 text 或 json");
            }
            runtime.import_event(&file, format == "json")?;
        }
        "integration-status" => {
            let manifest = take_value(&mut args, "--manifest")?.ok_or_else(|| {
                anyhow::anyhow!("integration-status 必须指定 --manifest 集成清单")
            })?;
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("integration-status --format 只支持 text 或 json");
            }
            runtime.integration_status(&manifest, format == "json")?;
        }
        "run-integration" => {
            let allow_pending_consumers =
                take_flag_before_separator(&mut args, "--allow-pending-consumers")?;
            let manifest = take_value_before_separator(&mut args, "--manifest")?
                .ok_or_else(|| anyhow::anyhow!("run-integration 必须指定 --manifest 集成清单"))?;
            let format = take_value_before_separator(&mut args, "--format")?
                .unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("run-integration --format 只支持 text 或 json");
            }
            if args.first().is_some_and(|arg| arg == "--") {
                args.remove(0);
            }
            forwarded = true;
            runtime.run_integration(&manifest, &args, format == "json", allow_pending_consumers)?;
        }
        "event-ack" => {
            let event_id = take_value(&mut args, "--event")?
                .ok_or_else(|| anyhow::anyhow!("event-ack 必须指定 --event 事件 ID"))?;
            let consumer = take_value(&mut args, "--consumer")?
                .ok_or_else(|| anyhow::anyhow!("event-ack 必须指定 --consumer 模块名"))?;
            let status = take_value(&mut args, "--status")?
                .ok_or_else(|| anyhow::anyhow!("event-ack 必须指定 --status 状态"))?;
            let message = take_value(&mut args, "--message")?;
            let receipt = take_value(&mut args, "--receipt")?;
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("event-ack --format 只支持 text 或 json");
            }
            runtime.acknowledge_event(
                &event_id,
                &consumer,
                &status,
                message.as_deref(),
                receipt.as_deref(),
                format == "json",
            )?;
        }
        "event-status" => {
            let event_id = take_value_before_separator(&mut args, "--event")?;
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("event-status --format 只支持 text 或 json");
            }
            runtime.event_status(event_id.as_deref(), format == "json")?;
        }
        "status" => runtime.print_status(take_flag(&mut args, "--json")?)?,
        "acquire-device" => {
            let serial = take_value(&mut args, "--serial")?;
            let any = take_flag(&mut args, "--any")?;
            if serial.is_some() == any {
                bail!("必须且只能指定 --serial <序列号> 或 --any");
            }
            let wait = take_value(&mut args, "--wait")?
                .unwrap_or_else(|| "0".into())
                .parse()?;
            let lease = take_value(&mut args, "--lease")?
                .unwrap_or_else(|| "3600".into())
                .parse()?;
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("acquire-device --format 只支持 text 或 json");
            }
            runtime.acquire_device(serial.as_deref(), wait, lease, format == "json")?;
        }
        "release-device" => runtime.release_device(take_value(&mut args, "--token")?.as_deref())?,
        "heartbeat-device" => runtime.heartbeat_device()?,
        "run-android-test" => {
            if args.first().is_some_and(|arg| arg == "--") {
                args.remove(0);
            }
            forwarded = true;
            runtime.run_android_test(&args)?;
        }
        "adb" => {
            if args.first().is_some_and(|arg| arg == "--") {
                args.remove(0);
            }
            forwarded = true;
            runtime.run_adb(&args)?;
        }
        "port" => {
            let name = take_value(&mut args, "--name")?
                .ok_or_else(|| anyhow::anyhow!("port 命令必须指定 --name"))?;
            let preferred = take_value(&mut args, "--preferred")?
                .map(|value| value.parse())
                .transpose()?;
            let base = take_value(&mut args, "--base")?
                .unwrap_or_else(|| "18000".into())
                .parse()?;
            let range = take_value(&mut args, "--range")?
                .unwrap_or_else(|| "1000".into())
                .parse()?;
            runtime.allocate_port(&name, preferred, base, range)?;
        }
        "ports" => runtime.list_ports(take_flag(&mut args, "--json")?)?,
        "lock-build" => {
            let timeout = take_value(&mut args, "--timeout")?
                .unwrap_or_else(|| "0".into())
                .parse()?;
            let lease = take_value(&mut args, "--lease")?
                .unwrap_or_else(|| "7200".into())
                .parse()?;
            let owner = take_value(&mut args, "--owner")?;
            runtime.lock_build(timeout, lease, owner.as_deref())?;
        }
        "unlock-build" => runtime.unlock_build(take_value(&mut args, "--token")?.as_deref())?,
        "run-gradle" => {
            let allow_clean = take_flag_before_separator(&mut args, "--allow-clean")?;
            let allow_primary = take_flag_before_separator(&mut args, "--allow-primary")?;
            let event_id = take_value_before_separator(&mut args, "--event")?;
            if args.first().is_some_and(|arg| arg == "--") {
                args.remove(0);
            }
            forwarded = true;
            runtime.run_gradle(&args, allow_clean, allow_primary, event_id.as_deref())?;
        }
        "install-cache" => {
            let snapshot = take_value(&mut args, "--snapshot")?
                .ok_or_else(|| anyhow::anyhow!("install-cache 必须指定 --snapshot 快照目录"))?;
            runtime.install_cache(&snapshot)?;
        }
        "cleanup" => {
            take_flag(&mut args, "--worktree")?;
            runtime.cleanup(
                take_flag(&mut args, "--dry-run")?,
                take_flag(&mut args, "--purge-cache")?,
                take_flag(&mut args, "--force")?,
                worktree_scope,
                worktree_path.as_deref(),
                worktree_branch.as_deref(),
            )?;
        }
        "doctor" => {
            let format = take_value(&mut args, "--format")?.unwrap_or_else(|| "text".into());
            if format != "text" && format != "json" {
                bail!("doctor --format 只支持 text 或 json");
            }
            runtime.doctor(take_flag(&mut args, "--json")? || format == "json")?;
        }
        other => bail!("未知命令：{}；运行 agentctl --help 查看用法", other),
    }
    if !forwarded && !args.is_empty() {
        bail!("未识别的多余参数：{}", args.join(" "));
    }
    Ok(())
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> Result<bool> {
    let indexes = args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == flag).then_some(index))
        .collect::<Vec<_>>();
    if indexes.len() > 1 {
        bail!("参数重复：{}", flag);
    }
    if let Some(index) = indexes.first() {
        args.remove(*index);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn take_value(args: &mut Vec<String>, flag: &str) -> Result<Option<String>> {
    let indexes = args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == flag).then_some(index))
        .collect::<Vec<_>>();
    if indexes.len() > 1 {
        bail!("参数重复：{}", flag);
    }
    if let Some(index) = indexes.first() {
        if *index + 1 >= args.len() {
            bail!("参数 {} 缺少值", flag);
        }
        let value = args.remove(*index + 1);
        args.remove(*index);
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

fn take_value_before_separator(args: &mut Vec<String>, flag: &str) -> Result<Option<String>> {
    let end = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let index = args[..end].iter().position(|arg| arg == flag);
    if let Some(index) = index {
        if index + 1 >= end {
            bail!("参数 {} 缺少值", flag);
        }
        let value = args.remove(index + 1);
        args.remove(index);
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

fn take_flag_before_separator(args: &mut Vec<String>, flag: &str) -> Result<bool> {
    let end = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let indexes = args[..end]
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == flag).then_some(index))
        .collect::<Vec<_>>();
    if indexes.len() > 1 {
        bail!("参数重复：{}", flag);
    }
    if let Some(index) = indexes.first() {
        args.remove(*index);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn print_help() {
    println!(
        "跨仓导入：event-import --file 事件文件 [--format text|json]；导入只登记事件，不执行候选代码。\n消费者通过确认：event-ack --status passed 还必须指定 --receipt 本机构建收据。"
    );
    println!(
        "跨仓消费者：contract-event 可加 --consumer-targets 路由JSON；标识须在事件内唯一，路由指定本机仓库绝对路径和 Gradle 项目。"
    );
    println!(
        "agentctl {}\n\n用法：agentctl <命令> [--lifecycle | --agent-id ID] [选项]\n\n命令：\n  prepare [--lifecycle] [--agent-id ID] [--allow-primary]\n  lifecycle-id\n  env [--format shell|powershell|json]\n  status [--json]\n  lock-build [--timeout 秒] [--lease 秒] [--owner 名称]\n  unlock-build [--token 令牌]\n  acquire-device (--serial 序列号 | --any) [--wait 秒] [--lease 秒] [--format text|json]\n  release-device [--token 令牌]\n  heartbeat-device\n  run-gradle [--lifecycle] [--agent-id ID] [--allow-primary] [--allow-clean] -- <Gradle 参数>\n  install-cache --snapshot 快照目录\n  run-android-test -- <Gradle 测试任务和参数>\n  adb -- <ADB 子命令和参数>\n  port --name 用途 [--preferred 端口] [--base 端口] [--range 数量]\n  ports [--json]\n  cleanup [--lifecycle | --agent-id ID] [--dry-run] [--purge-cache] [--force] [--worktree | --worktree-branch 分支 | --worktree-path 绝对路径]\n  doctor [--format text|json]\n",
        env!("CARGO_PKG_VERSION")
    );
    println!(
        "  candidate-version --base 版本\n  dependency-audit [--format text|json] [--enforce]\n  dependency-graph [--format text|json]\n  affected-consumers --provider 模块名 [--format text|json]\n  contract-snapshot [--format text|json] [--module 模块目录] [--output 文件]\n  contract-diff --baseline 文件 [--module 模块目录] [--format text|json] [--enforce]\n  contract-event --provider 模块名 --base 版本 [--module 模块目录] [--baseline 文件] [--format text|json]\n  event-inbox --consumer 模块名 [--format text|json]\n  event-ack --event 事件ID --consumer 模块名 --status received|validation_started|passed|failed [--message 文本] [--format text|json]\n  event-status [--event 事件ID] [--format text|json]\n  run-integration --manifest 集成清单 [--format text|json] -- <Gradle 参数>"
    );
    println!(
        "\n联合迁移：run-integration 可在 -- 前指定 --allow-pending-consumers，先测试候选组合；这不会确认消费者迁移或批准发布。\n  integration-status --manifest 集成清单 [--format text|json]"
    );
}
