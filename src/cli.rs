//! 在任何运行时副作用之前验证控制器参数；分隔符后的内容交给子命令验证。

use anyhow::{Result, bail};
use std::collections::HashSet;

pub fn validate(command: &str, args: &[String]) -> Result<()> {
    let (flags, values, forward): (&[&str], &[&str], bool) = match command {
        "prepare" => (&["--allow-primary"], &[], false),
        "env" => (&[], &["--format"], false),
        "status" | "ports" => (&["--json"], &[], false),
        "lifecycle-id" | "heartbeat-device" => (&[], &[], false),
        "candidate-version" => (&[], &["--base"], false),
        "dependency-audit" => (&["--enforce"], &["--format"], false),
        "dependency-graph" => (&[], &["--format"], false),
        "affected-consumers" => (&[], &["--provider", "--format"], false),
        "contract-snapshot" => (&[], &["--module", "--output", "--format"], false),
        "contract-diff" => (
            &["--enforce"],
            &["--baseline", "--module", "--format"],
            false,
        ),
        "contract-event" => (
            &[],
            &[
                "--provider",
                "--base",
                "--baseline",
                "--module",
                "--artifact-url",
                "--artifact-sha256",
                "--artifact-coordinate",
                "--consumer-targets",
                "--format",
            ],
            false,
        ),
        "event-inbox" => (&[], &["--consumer", "--format"], false),
        "event-import" => (&[], &["--file", "--format"], false),
        "event-ack" => (
            &[],
            &[
                "--event",
                "--consumer",
                "--status",
                "--message",
                "--receipt",
                "--format",
            ],
            false,
        ),
        "event-status" => (&[], &["--event", "--format"], false),
        "integration-status" => (&[], &["--manifest", "--format"], false),
        "run-integration" => (
            &["--allow-pending-consumers"],
            &["--manifest", "--format"],
            true,
        ),
        "lock-build" => (&[], &["--timeout", "--lease", "--owner"], false),
        "unlock-build" | "release-device" => (&[], &["--token"], false),
        "acquire-device" => (
            &["--any"],
            &["--serial", "--wait", "--lease", "--format"],
            false,
        ),
        "port" => (&[], &["--name", "--preferred", "--base", "--range"], false),
        "run-gradle" => (&["--allow-clean", "--allow-primary"], &["--event"], true),
        "run-android-test" | "adb" => (&[], &[], true),
        "install-cache" => (&[], &["--snapshot"], false),
        "cleanup" => (
            &["--dry-run", "--purge-cache", "--force", "--worktree"],
            &["--worktree-path", "--worktree-branch"],
            false,
        ),
        "doctor" => (&["--json"], &["--format"], false),
        _ => bail!("未知命令：{command}；运行 agentctl --help 查看用法"),
    };
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        return Ok(());
    }
    let mut seen = HashSet::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--" {
            if !forward {
                bail!("{command} 不接受转发参数");
            }
            break;
        }
        if !seen.insert(arg) {
            bail!("参数重复：{arg}");
        }
        let is_value = arg == "--agent-id" || values.contains(&arg);
        if is_value {
            let value = args
                .get(index + 1)
                .filter(|value| !value.is_empty() && !value.starts_with('-'));
            if value.is_none() {
                bail!("参数 {arg} 缺少有效值");
            }
            index += 2;
        } else if arg == "--lifecycle" || flags.contains(&arg) {
            index += 1;
        } else {
            // 不回显未知参数，避免误传的令牌进入错误日志。
            bail!("{command} 含未识别参数；Gradle 或 ADB 参数必须放在 -- 之后");
        }
    }
    if seen.contains("--lifecycle") && seen.contains("--agent-id") {
        bail!("--lifecycle 与 --agent-id 不能同时指定");
    }
    if seen.contains("--worktree-path") && seen.contains("--worktree-branch") {
        bail!("不能同时按 Worktree 路径和分支执行清理");
    }
    if command == "cleanup"
        && (seen.contains("--worktree")
            || seen.contains("--worktree-path")
            || seen.contains("--worktree-branch"))
        && (seen.contains("--agent-id") || seen.contains("--lifecycle"))
    {
        bail!("Worktree 清理不能同时指定单个 Agent 身份");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(command: &str, args: &[&str]) -> Result<()> {
        validate(
            command,
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }

    #[test]
    fn preflight_rejects_unknown_duplicate_and_missing_values() {
        assert!(check("prepare", &["--allow-primary", "--typo"]).is_err());
        assert!(check("cleanup", &["--force", "--force"]).is_err());
        assert!(check("prepare", &["--agent-id", "a", "--agent-id", "b"]).is_err());
        assert!(check("install-cache", &["--snapshot", "--lifecycle"]).is_err());
        assert!(check("prepare", &["--", "--allow-primary"]).is_err());
        assert!(check("unknown", &[]).is_err());
    }

    #[test]
    fn forwarding_is_explicit_and_does_not_parse_child_options() {
        assert!(check("run-gradle", &["--agent-id", "a", "--", "--info", "test"]).is_ok());
        assert!(check("adb", &["--", "shell", "echo", "--agent-id"]).is_ok());
        assert!(check("run-gradle", &["assemble"]).is_err());
        assert!(check("run-gradle", &["--allow-clena", "--", "clean"]).is_err());
    }

    #[test]
    fn identity_and_cleanup_selectors_cannot_be_mixed() {
        assert!(check("prepare", &["--lifecycle", "--agent-id", "a"]).is_err());
        assert!(check("cleanup", &["--worktree", "--agent-id", "a"]).is_err());
        assert!(
            check(
                "cleanup",
                &["--worktree-path", "x", "--worktree-branch", "y"]
            )
            .is_err()
        );
        assert!(check("cleanup", &["--worktree", "--worktree-path", "x"]).is_ok());
    }

    #[test]
    fn subcommand_help_is_accepted_without_runtime() {
        assert!(check("prepare", &["--help"]).is_ok());
        assert!(check("install-cache", &["-h"]).is_ok());
    }
}
