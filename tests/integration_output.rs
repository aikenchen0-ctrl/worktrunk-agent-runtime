#![cfg(windows)]

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const STDOUT_MARKER: &str = "simulated-integration-stdout";
const STDERR_MARKER: &str = "simulated-integration-stderr";

// 适配器只模拟进程输出与收据协议，不启动 Gradle，也不下载或解析制品。
const FAKE_WRAPPER: &str = "@echo off\r\npowershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"%~dp0simulate-integration.ps1\"\r\nexit /b %ERRORLEVEL%\r\n";

const FAKE_ADAPTER: &str = r#"$ErrorActionPreference = 'Stop'
[Console]::Out.WriteLine('simulated-integration-stdout')
[Console]::Error.WriteLine('simulated-integration-stderr')
if ($env:AGENT_TEST_INTEGRATION_MODE -eq 'fail') { exit 23 }
if ($env:AGENT_TEST_INTEGRATION_MODE -eq 'missing-evidence') { exit 0 }
if ($env:AGENT_TEST_INTEGRATION_MODE -ne 'success') { exit 24 }
$runtimeRoot = Split-Path -Parent (Split-Path -Parent $env:AGENT_VALIDATION_EVIDENCE)
$runDirectory = Join-Path (Join-Path $runtimeRoot 'integration-receipts') $env:AGENT_VALIDATION_EVENT_ID
$request = Get-Content -Raw -LiteralPath (Join-Path $runDirectory 'inputs.json') | ConvertFrom-Json
$manifest = $request.inputs.manifest
$candidates = @($request.inputs.events | ForEach-Object {
    @{ coordinate = $_.event.artifact.coordinate; sha256 = $_.event.artifact.sha256 }
})
$evidence = @{
    schema_version = 1
    integration_id = $request.integration_id
    input_sha256 = $request.input_sha256
    project = $manifest.project
    configuration = $manifest.configuration
    test_task = $manifest.test_task
    test_executed = $true
    tests = 2
    failures = 0
    skipped = 0
    graph_sha256 = ('b' * 64)
    candidates = $candidates
}
if ($manifest.build_task) {
    $evidence.build_task = $manifest.build_task
    $evidence.build_executed = $true
}
$encoding = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText([string]$request.evidence_path, ($evidence | ConvertTo-Json -Depth 20), $encoding)
exit 0
"#;

struct Fixture {
    root: TempDir,
    state: PathBuf,
    worktree: PathBuf,
    manifest: PathBuf,
}

fn isolated_command(program: &str, cwd: &Path, root: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("empty-git-config"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_CONFIG_COUNT");
    command
}

fn describe(output: &Output) -> String {
    format!(
        "退出状态：{}；标准输出：{}；标准错误：{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn success(output: Output) -> Output {
    assert!(output.status.success(), "命令应成功：{}", describe(&output));
    output
}

fn parse_complete_stdout(output: &Output) -> Value {
    // 必须解析完整输出，禁止通过截取首个大括号掩盖日志污染。
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!("完整标准输出必须是单个 JSON：{error}；{}", describe(output))
    })
}

fn write_file(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("测试文件应有父目录")).expect("创建测试目录失败");
    fs::write(path, text).expect("写入测试文件失败");
}

fn write_json(path: &Path, value: &Value) {
    write_file(
        path,
        &serde_json::to_string_pretty(value).expect("序列化测试数据失败"),
    );
}

fn git(root: &Path, cwd: &Path, args: &[&str]) -> Output {
    success(
        isolated_command("git", cwd, root)
            .args([
                "-c",
                "user.name=Integration Output Test",
                "-c",
                "user.email=integration-output@example.invalid",
                "-c",
                "commit.gpgSign=false",
            ])
            .args(args)
            .output()
            .expect("执行测试 Git 命令失败"),
    )
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("agentctl-integration-output-")
            .tempdir()
            .expect("创建隔离测试目录失败");
        let repository = root.path().join("repository");
        let worktree = root.path().join("integration-worktree");
        let state = root.path().join("state");
        let manifest = root.path().join("integration-manifest.json");
        fs::create_dir_all(&repository).expect("创建测试仓库失败");
        git(root.path(), &repository, &["init", "-b", "main"]);
        write_file(&repository.join("gradlew.bat"), FAKE_WRAPPER);
        write_file(&repository.join("simulate-integration.ps1"), FAKE_ADAPTER);
        write_file(
            &repository.join("settings.gradle.kts"),
            "rootProject.name = \"simulated-integration\"\n",
        );
        git(root.path(), &repository, &["add", "."]);
        git(
            root.path(),
            &repository,
            &["commit", "-m", "simulated integration fixture"],
        );
        git(
            root.path(),
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "integration-output",
                worktree.to_str().expect("测试路径应为 UTF-8"),
            ],
        );
        write_json(
            &manifest,
            &json!({
                "schema_version": 1,
                "events": ["simulated-candidate"],
                "project": ":",
                "configuration": "testRuntimeClasspath",
                "test_task": ":test",
                "build_task": ":assemble"
            }),
        );
        write_json(
            &state.join("events/simulated-candidate.json"),
            &json!({
                "schema_version": 1,
                "event_id": "simulated-candidate",
                "event_type": "contract_candidate",
                "provider": "simulated-provider",
                "repository": repository,
                "candidate_version": "1.0.0-dev.abc",
                "affected_consumers": [],
                "routing_schema_version": 1,
                "consumer_targets": {},
                "artifact": {
                    "coordinate": "example:simulated:1.0.0-dev.abc",
                    "sha256": "a".repeat(64),
                    "url": "https://example.invalid/simulated.jar"
                }
            }),
        );
        let fixture = Self {
            root,
            state,
            worktree,
            manifest,
        };
        success(fixture.cli(&["prepare"], "success"));
        fixture
    }

    fn cli(&self, args: &[&str], mode: &str) -> Output {
        let mut command = isolated_command(
            env!("CARGO_BIN_EXE_agentctl"),
            &self.worktree,
            self.root.path(),
        );
        command
            .env("AGENT_RUNTIME_ROOT", &self.state)
            .env("AGENT_ID", "integration-output-test")
            .env("AGENT_TEST_INTEGRATION_MODE", mode)
            .env("AGENT_COMMAND_TIMEOUT_SECONDS", "60")
            .env_remove("AGENT_MAX_WORKERS")
            .env_remove("AGENT_MAX_HEAP")
            .env_remove("AGENT_CPU_PERCENT")
            .env_remove("AGENT_MEMORY_LIMIT_MB")
            .env_remove("AGENT_BUILD_LOCK_TOKEN")
            .args(args)
            .output()
            .expect("执行 agentctl 测试命令失败")
    }

    fn run(&self, mode: &str) -> Output {
        self.cli(
            &[
                "run-integration",
                "--manifest",
                self.manifest.to_str().expect("测试路径应为 UTF-8"),
                "--format",
                "json",
            ],
            mode,
        )
    }

    fn status(&self) -> Output {
        self.cli(
            &[
                "integration-status",
                "--manifest",
                self.manifest.to_str().expect("测试路径应为 UTF-8"),
                "--format",
                "json",
            ],
            "success",
        )
    }

    fn assert_streams_logs_and_release(&self, output: &Output, receipt: &Value, exit_code: i32) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for marker in [
            STDOUT_MARKER,
            STDERR_MARKER,
            "Gradle 退出码",
            "构建锁已释放",
        ] {
            assert!(
                stderr.contains(marker),
                "诊断应写入标准错误，缺少 {marker}：{}",
                describe(output)
            );
            assert!(
                !stdout.contains(marker),
                "诊断不得混入 JSON 标准输出：{marker}"
            );
        }
        assert_eq!(receipt["build_receipt"]["exit_code"], exit_code);
        assert_eq!(receipt["build_receipt"]["worktree_clean"], true);
        let stdout_log = Path::new(
            receipt["build_receipt"]["stdout_log"]
                .as_str()
                .expect("应保存标准输出日志路径"),
        );
        let stderr_log = Path::new(
            receipt["build_receipt"]["stderr_log"]
                .as_str()
                .expect("应保存标准错误日志路径"),
        );
        let expected_root = fs::canonicalize(self.root.path()).expect("解析测试根目录失败");
        for path in [stdout_log, stderr_log] {
            assert!(
                fs::canonicalize(path)
                    .expect("日志文件应存在")
                    .starts_with(&expected_root),
                "日志必须位于隔离测试目录"
            );
        }
        let raw_stdout = fs::read_to_string(stdout_log).expect("读取原始标准输出日志失败");
        let raw_stderr = fs::read_to_string(stderr_log).expect("读取原始标准错误日志失败");
        assert!(
            raw_stdout.contains(STDOUT_MARKER),
            "原始标准输出日志应保存子进程输出"
        );
        assert!(
            raw_stderr.contains(STDERR_MARKER),
            "原始标准错误日志应保存子进程错误输出"
        );
        assert!(
            !raw_stdout.contains(STDERR_MARKER),
            "原始输出日志不得混入另一管道"
        );
        assert!(
            !raw_stderr.contains(STDOUT_MARKER),
            "显示流重定向不得改变原始日志归属"
        );
        let paths = &receipt["metadata"]["paths"];
        let worktree_root = Path::new(paths["worktree_root"].as_str().expect("应有工作树状态目录"));
        let runtime_root = Path::new(paths["root"].as_str().expect("应有运行时状态目录"));
        assert!(
            !worktree_root.join("locks/build.json").exists(),
            "构建完成后必须释放构建锁"
        );
        assert!(
            !runtime_root.join("locks/build.token").exists(),
            "构建完成后必须清理锁令牌"
        );
        let clean = git(
            self.root.path(),
            &self.worktree,
            &["status", "--porcelain", "--untracked-files=all"],
        );
        assert!(clean.stdout.is_empty(), "模拟进程不得修改测试工作树");
    }
}

#[test]
fn failed_simulated_process_returns_json_and_preserves_diagnostics_and_logs() {
    let fixture = Fixture::new();
    let output = fixture.run("fail");
    assert!(!output.status.success(), "模拟退出码 23 必须使集成失败");
    let receipt = parse_complete_stdout(&output);
    assert_eq!(receipt["success"], false);
    assert_eq!(receipt["ready_for_integration"], false);
    assert_eq!(receipt["release_approved"], false);
    assert_eq!(receipt["evidence"], Value::Null);
    assert!(
        receipt["error"]
            .as_str()
            .expect("失败收据应有原因")
            .contains("23")
    );
    fixture.assert_streams_logs_and_release(&output, &receipt, 23);
    let status = fixture.status();
    assert!(!status.status.success(), "失败组合不得显示为已就绪");
    let report = parse_complete_stdout(&status);
    assert_eq!(report["combination_verified"], false);
    assert_eq!(report["ready_for_integration"], false);
}

#[test]
fn successful_simulated_evidence_returns_clean_json_and_releases_the_lock() {
    let fixture = Fixture::new();
    let output = success(fixture.run("success"));
    let receipt = parse_complete_stdout(&output);
    assert_eq!(receipt["success"], true);
    assert_eq!(receipt["ready_for_integration"], true);
    assert_eq!(receipt["release_approved"], false);
    assert_eq!(
        receipt["evidence"]["integration_id"],
        receipt["integration_id"]
    );
    assert_eq!(receipt["evidence"]["input_sha256"], receipt["input_sha256"]);
    assert_eq!(receipt["evidence"]["test_executed"], true);
    assert_eq!(receipt["evidence"]["build_executed"], true);
    fixture.assert_streams_logs_and_release(&output, &receipt, 0);
    let report = parse_complete_stdout(&success(fixture.status()));
    assert_eq!(report["combination_verified"], true);
    assert_eq!(report["ready_for_integration"], true);
    assert_eq!(report["release_approved"], false);
}

#[test]
fn zero_exit_without_simulated_evidence_is_rejected_with_parseable_json() {
    let fixture = Fixture::new();
    let output = fixture.run("missing-evidence");
    assert!(!output.status.success(), "零退出码不能替代组合验证证据");
    let receipt = parse_complete_stdout(&output);
    assert_eq!(receipt["success"], false);
    assert_eq!(receipt["ready_for_integration"], false);
    assert_eq!(receipt["build_receipt"]["success"], true);
    assert_eq!(receipt["evidence"], Value::Null);
    assert!(
        receipt["error"]
            .as_str()
            .expect("失败收据应有原因")
            .contains("缺少本次组合制品和测试证据")
    );
    fixture.assert_streams_logs_and_release(&output, &receipt, 0);
}
