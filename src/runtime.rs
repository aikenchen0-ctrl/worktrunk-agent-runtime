//! 为单个 Git Worktree 隔离本机 Agent 运行状态。
//!
//! 所有可变状态位于用户状态目录，而不是仓库内；Worktree 身份来自 Git 的专属管理目录，
//! 因此切换分支、重启 Codex 或更改 Agent 名称不会意外切换到另一份 Gradle/Maven 缓存。

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration as StdDuration, Instant};
use tempfile::NamedTempFile;

const SCHEMA_VERSION: u32 = 1;
const APP_DIR: &str = "agent-runtime";

#[path = "integration.rs"]
mod integration;
#[path = "probe.rs"]
mod probe;
#[path = "routing.rs"]
mod routing;

#[derive(Debug, Clone)]
pub struct Runtime {
    cwd: PathBuf,
    state_root: PathBuf,
    agent_id: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct PrepareOptions {
    pub allow_primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub schema_version: u32,
    pub runtime_id: String,
    pub agent_id: String,
    pub worktree_id: String,
    pub repository: String,
    pub worktree_path: String,
    pub branch: Option<String>,
    pub commit: String,
    pub is_primary: bool,
    pub created_at: String,
    pub last_seen_at: String,
    pub status: String,
    pub paths: Paths,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paths {
    pub root: String,
    pub worktree_root: String,
    pub gradle_user_home: String,
    pub maven_local: String,
    pub temp: String,
    pub logs: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildLock {
    schema_version: u32,
    runtime_id: String,
    owner: String,
    pid: u32,
    process_start_time: String,
    token: String,
    created_at: String,
    expires_at: String,
    heartbeat_at: String,
    #[serde(default)]
    gradle_pid: Option<u32>,
    #[serde(default)]
    gradle_process_start_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceLease {
    schema_version: u32,
    runtime_id: String,
    agent_id: String,
    serial: String,
    pid: u32,
    process_start_time: String,
    token: String,
    acquired_at: String,
    expires_at: String,
    heartbeat_at: String,
    lease_seconds: u64,
    status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TestLease {
    schema_version: u32,
    runtime_id: String,
    agent_id: String,
    serial: String,
    pid: u32,
    process_start_time: String,
    token: String,
    acquired_at: String,
    expires_at: String,
    heartbeat_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildReceipt {
    schema_version: u32,
    runtime_id: String,
    agent_id: String,
    worktree_id: String,
    repository: String,
    worktree_path: String,
    commit: String,
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    artifact_sha256: Option<String>,
    #[serde(default)]
    validation: Option<CandidateValidation>,
    #[serde(default)]
    worktree_clean: bool,
    command: Vec<String>,
    started_at: String,
    finished_at: String,
    elapsed_ms: u128,
    exit_code: Option<i32>,
    success: bool,
    stdout_log: String,
    stderr_log: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CandidateValidation {
    schema_version: u32,
    run_id: String,
    attempt_id: String,
    event_id: String,
    consumer: String,
    coordinate: String,
    artifact_sha256: String,
    project: String,
    configuration: String,
    task: String,
    tests: u64,
    failures: u64,
    skipped: u64,
}

#[derive(Debug, Clone)]
pub struct ArtifactEvidence {
    pub url: Option<String>,
    pub sha256: Option<String>,
    pub coordinate: Option<String>,
}

pub struct ContractEventOptions<'a> {
    pub provider: &'a str,
    pub base: &'a str,
    pub baseline: Option<&'a str>,
    pub module: Option<&'a str>,
    pub artifact: Option<&'a ArtifactEvidence>,
    pub consumer_targets: Option<&'a str>,
    pub json: bool,
}

struct StateFileLock {
    path: PathBuf,
    token: String,
}

struct ManagedChild {
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}

struct ProcessLimits {
    cpu_percent: u32,
    memory_bytes: Option<usize>,
}

impl ManagedChild {
    fn attach(child: &std::process::Child, limits: &ProcessLimits) -> Result<Self> {
        #[cfg(windows)]
        {
            use std::mem::size_of;
            use windows_sys::Win32::System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
                JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_CPU_RATE_CONTROL_INFORMATION_0,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectCpuRateControlInformation,
                JobObjectExtendedLimitInformation, SetInformationJobObject,
            };
            use windows_sys::Win32::System::Threading::{
                OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
            };

            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                bail!("创建 Gradle 子进程作业对象失败");
            }
            let mut job_limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            job_limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&job_limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
                bail!("配置 Gradle 子进程作业对象失败");
            }
            let cpu_cap = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
                ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE
                    | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
                Anonymous: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION_0 {
                    CpuRate: limits.cpu_percent * 100,
                },
            };
            let configured = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectCpuRateControlInformation,
                    (&cpu_cap as *const JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                    size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
                bail!("配置 Gradle CPU 上限失败");
            }
            if let Some(bytes) = limits.memory_bytes {
                let mut memory_limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                memory_limits.BasicLimitInformation.LimitFlags =
                    windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                        | windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_JOB_MEMORY;
                memory_limits.JobMemoryLimit = bytes;
                let configured = unsafe {
                    SetInformationJobObject(
                        job,
                        JobObjectExtendedLimitInformation,
                        (&memory_limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    )
                };
                if configured == 0 {
                    unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
                    bail!("配置 Gradle 进程树内存上限失败");
                }
            }
            let process =
                unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, child.id()) };
            if process.is_null() {
                unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
                bail!("打开 Gradle Wrapper 进程失败");
            }
            let assigned = unsafe { AssignProcessToJobObject(job, process) };
            unsafe { windows_sys::Win32::Foundation::CloseHandle(process) };
            if assigned == 0 {
                unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
                bail!("将 Gradle Wrapper 加入受控进程树失败");
            }
            Ok(Self { job })
        }
        #[cfg(not(windows))]
        {
            let _ = (child, limits);
            Ok(Self {})
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job);
        }
    }
}

impl StateFileLock {
    fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let started = Instant::now();
        loop {
            recover_state_lock_if_stale(path)?;
            let token = unique_token();
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut file) => {
                    writeln!(
                        file,
                        "{}\n{}\n{}",
                        std::process::id(),
                        process_start_time(),
                        token
                    )?;
                    file.sync_all()?;
                    return Ok(Self {
                        path: path.to_path_buf(),
                        token,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Ok(contents) = fs::read_to_string(path) {
                        let mut lines = contents.lines();
                        let pid = lines.next().and_then(|value| value.parse::<u32>().ok());
                        let started_at = lines.next();
                        let token = lines.next();
                        if let (Some(pid), Some(started_at), Some(token)) = (pid, started_at, token)
                        {
                            if !process_identity_alive(pid, started_at) {
                                let current = fs::read_to_string(path).unwrap_or_default();
                                if current.lines().nth(2) == Some(token) {
                                    let _ = fs::remove_file(path);
                                }
                                continue;
                            }
                        }
                    }
                    if started.elapsed() >= StdDuration::from_secs(30) {
                        bail!("等待状态互斥锁超时：{}", path.display());
                    }
                    thread::sleep(StdDuration::from_millis(50));
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("获取状态互斥锁失败：{}", path.display()));
                }
            }
        }
    }
}

impl Drop for StateFileLock {
    fn drop(&mut self) {
        if let Ok(contents) = fs::read_to_string(&self.path) {
            if contents.lines().nth(2) == Some(self.token.as_str()) {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PortLease {
    schema_version: u32,
    runtime_id: String,
    owner: String,
    name: String,
    port: u16,
    created_at: String,
    expires_at: String,
    status: String,
}

impl Runtime {
    pub fn discover() -> Result<Self> {
        let cwd = env::current_dir().context("无法获取当前目录")?;
        let state_root = if let Some(configured) = env::var_os("AGENT_RUNTIME_ROOT") {
            let root = PathBuf::from(configured);
            if !root.is_absolute()
                || root
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
                || root.parent().is_none()
            {
                bail!("AGENT_RUNTIME_ROOT 必须是非磁盘根目录的绝对路径，且不能包含上级跳转");
            }
            root
        } else {
            let state_base = env::var_os("LOCALAPPDATA")
                .or_else(|| env::var_os("XDG_DATA_HOME"))
                .or_else(|| {
                    env::var_os("HOME")
                        .map(|home| PathBuf::from(home).join(".local/share").into_os_string())
                })
                .ok_or_else(|| anyhow::anyhow!("未找到 LOCALAPPDATA、XDG_DATA_HOME 或 HOME"))?;
            PathBuf::from(state_base).join(APP_DIR)
        };
        Ok(Self {
            cwd,
            state_root,
            agent_id: env::var("AGENT_ID").ok(),
        })
    }

    #[cfg(test)]
    fn with_paths(cwd: PathBuf, state_root: PathBuf) -> Self {
        Self {
            cwd,
            state_root,
            agent_id: Some("test-agent".to_string()),
        }
    }

    pub fn with_agent_id(mut self, agent_id: &str) -> Result<Self> {
        if !valid_agent_id(agent_id) {
            bail!("Agent ID 只能包含 ASCII 字母、数字、下划线和连字符，长度不得超过 64");
        }
        self.agent_id = Some(agent_id.to_string());
        Ok(self)
    }

    pub fn discover_for_agent(agent_id: Option<&str>) -> Result<Self> {
        let runtime = Self::discover()?;
        if let Some(agent_id) = agent_id {
            return runtime.with_agent_id(agent_id);
        }
        if runtime.agent_id.as_deref().is_some_and(valid_agent_id) {
            return Ok(runtime);
        }
        let git = runtime.git_info()?;
        let id = worktree_id(&git.git_dir);
        runtime.with_agent_id(&format!("codex-{id}"))
    }

    pub fn lifecycle_agent_id(cwd: &Path) -> Result<String> {
        let git_dir = PathBuf::from(git_output(cwd, &["rev-parse", "--absolute-git-dir"])?);
        Ok(format!("worktrunk-{}", worktree_id(&git_dir)))
    }

    pub fn discover_for_worktree() -> Result<Self> {
        Self::discover()
    }

    pub fn prepare(&self, options: PrepareOptions) -> Result<Metadata> {
        let git = self.git_info()?;
        if git.is_primary && !options.allow_primary {
            bail!("当前目录是主 Worktree，默认拒绝准备运行时；确需使用时传入 --allow-primary");
        }
        let runtime_id = worktree_id(&git.git_dir);
        let worktree_root = self.state_root.join("worktrees").join(&runtime_id);
        fs::create_dir_all(&worktree_root)?;
        let agent_id = match self.agent_id.clone() {
            Some(value) if valid_agent_id(&value) => value,
            Some(_) => bail!("AGENT_ID 只能包含 ASCII 字母、数字、下划线和连字符"),
            None => format!("codex-{}", runtime_id),
        };
        let agent_root = self
            .state_root
            .join("agents")
            .join(safe_component(&agent_id))
            .join(&runtime_id);
        let first_prepare = !agent_root.join("metadata.json").exists();
        let paths = Paths {
            root: agent_root.display().to_string(),
            worktree_root: worktree_root.display().to_string(),
            gradle_user_home: worktree_root.join("gradle-user-home").display().to_string(),
            maven_local: worktree_root.join("maven-local").display().to_string(),
            temp: agent_root.join("temp").display().to_string(),
            logs: agent_root.join("logs").display().to_string(),
        };
        for path in [
            PathBuf::from(&paths.worktree_root),
            PathBuf::from(&paths.gradle_user_home),
            PathBuf::from(&paths.maven_local),
            PathBuf::from(&paths.worktree_root).join("locks"),
            PathBuf::from(&paths.root),
            PathBuf::from(&paths.temp),
            PathBuf::from(&paths.logs),
            PathBuf::from(&paths.root).join("locks"),
        ] {
            fs::create_dir_all(&path)
                .with_context(|| format!("创建运行目录失败：{}", path.display()))?;
        }

        let path = agent_root.join("metadata.json");
        let prior = read_json::<Metadata>(&path).ok();
        let worktree_record_path = worktree_root.join("worktree.json");
        let _worktree_guard = StateFileLock::acquire(&worktree_root.join("locks/worktree.lock"))?;
        let _build_guard = StateFileLock::acquire(&worktree_root.join("locks/build-gate.lock"))?;
        let old_worktree_record: Option<serde_json::Value> = read_json(&worktree_record_path).ok();
        if old_worktree_record
            .as_ref()
            .and_then(|record| record.get("status"))
            .and_then(serde_json::Value::as_str)
            == Some("removed")
        {
            bail!("Worktree runtime 已标记删除，不能复活相同 Worktree 身份");
        }
        let now = Utc::now().to_rfc3339();
        let metadata = Metadata {
            schema_version: SCHEMA_VERSION,
            runtime_id: runtime_id.clone(),
            agent_id,
            worktree_id: runtime_id.clone(),
            repository: git.repository.display().to_string(),
            worktree_path: git.worktree.display().to_string(),
            branch: git.branch,
            commit: git.commit,
            is_primary: git.is_primary,
            created_at: prior
                .map(|old| old.created_at)
                .unwrap_or_else(|| now.clone()),
            last_seen_at: now,
            status: "active".to_string(),
            paths,
        };
        write_json_atomic(&path, &metadata)?;
        let worktree_record = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "runtime_id": runtime_id,
            "repository": git.repository,
            "worktree_path": git.worktree,
            "branch": metadata.branch,
            "commit": metadata.commit,
            "created_at": old_worktree_record.as_ref().and_then(|record| record.get("created_at")).cloned().unwrap_or_else(|| serde_json::json!(metadata.created_at)),
            "last_seen_at": metadata.last_seen_at,
            "status": "active"
        });
        write_json_atomic(&worktree_record_path, &worktree_record)?;
        let local_ports_path = agent_root.join("ports.json");
        let global_ports_path = self.state_root.join("ports.json");
        if first_prepare || !local_ports_path.exists() {
            fs::create_dir_all(&self.state_root)?;
            let registry_lock = global_ports_path.with_extension("lock");
            recover_state_lock_if_stale(&registry_lock)?;
            let _guard = StateFileLock::acquire(&registry_lock)?;
            let mut registry = load_port_registry(&global_ports_path)?;
            registry.retain(|entry| {
                !port_expired(entry) && worktree_is_active(&self.state_root, &entry.runtime_id)
            });
            let expires_at = (Utc::now() + Duration::days(7)).to_rfc3339();
            for entry in registry.iter_mut().filter(|entry| {
                entry.runtime_id == metadata.runtime_id && entry.owner == metadata.agent_id
            }) {
                entry.status = "active".to_string();
                entry.expires_at = expires_at.clone();
            }
            write_json_atomic(&global_ports_path, &registry)?;
            let local = registry
                .iter()
                .filter(|entry| {
                    entry.runtime_id == metadata.runtime_id && entry.owner == metadata.agent_id
                })
                .cloned()
                .collect::<Vec<_>>();
            write_json_atomic(&local_ports_path, &local)?;
        }
        write_text_atomic(
            &agent_root.join("worktree.env"),
            &format!("AGENT_RUNTIME_ID={}\n", metadata.runtime_id),
        )?;
        Ok(metadata)
    }

    pub fn print_env(&self, format: &str) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        let leased_serial = read_json::<DeviceLease>(
            &PathBuf::from(&metadata.paths.root).join("device-lease.json"),
        )
        .ok()
        .filter(|lease| {
            lease.runtime_id == metadata.runtime_id && !lock_expired_fields(&lease.expires_at)
        })
        .map(|lease| lease.serial);
        let values = serde_json::json!({
            "AGENT_ID": metadata.agent_id,
            "WORKTREE_ID": metadata.worktree_id,
            "AGENT_RUNTIME_ID": metadata.runtime_id,
            "AGENT_RUNTIME_ROOT": self.state_root,
            "GRADLE_USER_HOME": metadata.paths.gradle_user_home,
            "MAVEN_REPO_LOCAL": metadata.paths.maven_local,
            "ANDROID_SERIAL": leased_serial.unwrap_or_default(),
        });
        match format {
            "json" => println!("{}", serde_json::to_string_pretty(&values)?),
            "powershell" => {
                for (key, value) in values.as_object().expect("JSON object") {
                    println!(
                        "$env:{} = {}",
                        key,
                        ps_quote(value.as_str().unwrap_or_default())
                    );
                }
            }
            "shell" => {
                for (key, value) in values.as_object().expect("JSON object") {
                    println!(
                        "export {}={}",
                        key,
                        shell_quote(value.as_str().unwrap_or_default())
                    );
                }
            }
            other => bail!("不支持的环境格式：{}", other),
        }
        Ok(())
    }

    pub fn print_status(&self, json: bool) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        let lock = read_json::<BuildLock>(
            &PathBuf::from(&metadata.paths.worktree_root).join("locks/build.json"),
        )
        .ok()
        .filter(|item| item.owner == metadata.agent_id);
        let worktree = read_json::<serde_json::Value>(
            &PathBuf::from(&metadata.paths.worktree_root).join("worktree.json"),
        )
        .ok();
        let device_lease = read_json::<DeviceLease>(
            &PathBuf::from(&metadata.paths.root).join("device-lease.json"),
        )
        .ok();
        let lock_view = lock.map(|item| serde_json::json!({"owner": item.owner, "pid": item.pid, "created_at": item.created_at, "expires_at": item.expires_at, "heartbeat_at": item.heartbeat_at}));
        let device_view = device_lease.map(|item| serde_json::json!({"agent_id": item.agent_id, "serial": item.serial, "acquired_at": item.acquired_at, "expires_at": item.expires_at, "heartbeat_at": item.heartbeat_at, "status": item.status}));
        let result = serde_json::json!({"metadata": metadata, "worktree": worktree, "build_lock": lock_view, "device_lease": device_view});
        if json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!(
                "运行时：{}\nWorktree：{}\n状态：{}\n构建锁：{}",
                result["metadata"]["runtime_id"],
                result["metadata"]["worktree_path"],
                result["metadata"]["status"],
                if result["build_lock"].is_null() {
                    "空闲"
                } else {
                    "占用"
                }
            );
        }
        Ok(())
    }

    pub fn candidate_version(&self, base: &str) -> Result<String> {
        if !fixed_version(base) {
            bail!("候选基础版本必须是固定 ASCII 版本，不得含动态选择符、空白或 SNAPSHOT");
        }
        let commit = self.git_info()?.commit;
        let short = commit.chars().take(12).collect::<String>();
        Ok(format!("{base}-dev.{short}"))
    }

    pub fn dependency_audit(&self, json: bool, enforce: bool) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        let mut findings = Vec::new();
        let mut files = Vec::new();
        collect_files(&self.cwd, &mut files)?;
        for path in files {
            let is_gradle = path.file_name().is_some_and(|name| {
                name == "build.gradle"
                    || name == "build.gradle.kts"
                    || name == "settings.gradle"
                    || name == "settings.gradle.kts"
                    || name == "gradle.properties"
            });
            if !is_gradle {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap_or_default();
            for (line_number, line) in text.lines().enumerate() {
                let trimmed = line.trim();
                let rule = if trimmed.contains("mavenLocal()") {
                    Some(("maven-local", "禁止把本机 Maven 仓库作为跨仓依赖来源"))
                } else if trimmed.contains("SNAPSHOT") {
                    Some(("snapshot", "禁止使用可覆盖的 SNAPSHOT 版本"))
                } else if trimmed.contains("implementation(files(")
                    || trimmed.contains("api(files(")
                {
                    Some((
                        "file-aar",
                        "文件型 AAR 不可追踪候选版本和依赖图；应发布带 SHA 的 Maven 制品",
                    ))
                } else if trimmed.contains("project(\"") || trimmed.contains("project('") {
                    Some((
                        "project-dependency",
                        "当前模块依赖同仓源码；拆仓后必须替换为不可变制品坐标",
                    ))
                } else {
                    None
                };
                if let Some((code, message)) = rule {
                    findings.push(serde_json::json!({
                        "code": code,
                        "message": message,
                        "file": path.strip_prefix(&self.cwd).unwrap_or(&path).display().to_string(),
                        "line": line_number + 1,
                        "text": trimmed,
                    }));
                }
            }
        }
        let report = serde_json::json!({
            "worktree_id": metadata.worktree_id,
            "commit": metadata.commit,
            "candidate_version_example": self.candidate_version("0.1.0")?,
            "findings": findings,
        });
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else if report["findings"]
            .as_array()
            .is_some_and(|items| items.is_empty())
        {
            println!("依赖审计通过：未发现高风险本地依赖规则");
        } else {
            for item in report["findings"].as_array().expect("findings array") {
                println!(
                    "{}:{} [{}] {}",
                    item["file"], item["line"], item["code"], item["message"]
                );
            }
        }
        if enforce
            && report["findings"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        {
            bail!(
                "依赖审计发现 {} 个问题",
                report["findings"].as_array().unwrap().len()
            );
        }
        Ok(())
    }

    pub fn dependency_graph(&self, json: bool) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        let edges = self
            .dependency_edges()?
            .into_iter()
            .map(|(consumer, provider, file)| {
                serde_json::json!({"consumer": consumer, "provider": provider, "file": file})
            })
            .collect::<Vec<_>>();
        let report = serde_json::json!({
            "repository": metadata.repository,
            "commit": metadata.commit,
            "edges": edges,
        });
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else if let Some(items) = report["edges"].as_array() {
            for edge in items {
                println!("{} -> {}", edge["consumer"], edge["provider"]);
            }
        }
        Ok(())
    }

    fn dependency_edges(&self) -> Result<Vec<(String, String, String)>> {
        let mut edges = Vec::new();
        let mut files = Vec::new();
        collect_files(&self.cwd, &mut files)?;
        for path in files {
            if !path
                .file_name()
                .is_some_and(|name| name == "build.gradle" || name == "build.gradle.kts")
            {
                continue;
            }
            let Some(module) = path.parent().and_then(|dir| dir.file_name()) else {
                continue;
            };
            let text = fs::read_to_string(&path).unwrap_or_default();
            for line in text.lines() {
                for marker in ["project(\"", "project('"] {
                    let Some(start) = line.find(marker) else {
                        continue;
                    };
                    let rest = &line[start + marker.len()..];
                    let delimiter = marker.chars().last().expect("project marker");
                    let Some(end) = rest.find(delimiter) else {
                        continue;
                    };
                    edges.push((
                        module.to_string_lossy().to_string(),
                        rest[..end].trim_start_matches(':').to_string(),
                        path.strip_prefix(&self.cwd)
                            .unwrap_or(&path)
                            .display()
                            .to_string(),
                    ));
                    break;
                }
            }
        }
        Ok(edges)
    }

    pub fn affected_consumers(&self, provider: &str, json: bool) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        let consumers = self.affected_consumer_names(provider)?;
        let result = serde_json::json!({
            "provider": provider,
            "consumers": consumers.iter().collect::<Vec<_>>(),
            "commit": metadata.commit,
        });
        if json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            for consumer in &consumers {
                println!("{}", consumer);
            }
        }
        Ok(())
    }

    fn affected_consumer_names(
        &self,
        provider: &str,
    ) -> Result<std::collections::BTreeSet<String>> {
        let mut consumers = self.local_affected_consumer_names(provider)?;
        consumers.extend(self.declared_consumer_names(provider)?);
        Ok(consumers)
    }

    fn local_affected_consumer_names(
        &self,
        provider: &str,
    ) -> Result<std::collections::BTreeSet<String>> {
        let edges = self.dependency_edges()?;
        let mut consumers = std::collections::BTreeSet::new();
        let mut pending = vec![provider.trim_start_matches(':').to_string()];
        let mut visited = std::collections::BTreeSet::new();
        while let Some(current) = pending.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            for (consumer, dependency, _) in &edges {
                if dependency == &current && consumers.insert(consumer.clone()) {
                    pending.push(consumer.clone());
                }
            }
        }
        Ok(consumers)
    }

    fn declared_consumer_names(
        &self,
        provider: &str,
    ) -> Result<std::collections::BTreeSet<String>> {
        let path = self.cwd.join("config/dependents.yml");
        if !path.is_file() {
            return Ok(std::collections::BTreeSet::new());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("读取跨仓消费者登记失败：{}", path.display()))?;
        let mut declared = std::collections::BTreeSet::new();
        let mut provider_matches = false;
        let mut in_consumers = false;
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(value) = line.strip_prefix("provider:") {
                provider_matches = value.trim().trim_matches(['"', '\'']) == provider;
                in_consumers = false;
                continue;
            }
            if line == "consumers:" {
                in_consumers = provider_matches
                    || !text
                        .lines()
                        .any(|item| item.trim().starts_with("provider:"));
                continue;
            }
            if !in_consumers {
                continue;
            }
            let value = line
                .strip_prefix("-")
                .map(str::trim)
                .or_else(|| line.strip_prefix("name:").map(str::trim))
                .or_else(|| line.strip_prefix("module:").map(str::trim));
            let Some(value) = value else {
                continue;
            };
            let value = value
                .strip_prefix("name:")
                .or_else(|| value.strip_prefix("module:"))
                .unwrap_or(value)
                .trim()
                .trim_matches(['"', '\'']);
            if valid_agent_id(value) {
                declared.insert(value.to_string());
            }
        }
        Ok(declared)
    }

    pub fn create_contract_event(&self, options: ContractEventOptions<'_>) -> Result<()> {
        let ContractEventOptions {
            provider,
            base,
            baseline,
            module,
            artifact,
            consumer_targets,
            json,
        } = options;
        let metadata = self.ensure_metadata(false)?;
        let status = git_output(
            &self.cwd,
            &["status", "--porcelain", "--untracked-files=all"],
        )?;
        if !status.is_empty() {
            bail!("工作区存在未提交改动；先提交接口变更，再生成与提交 SHA 对应的候选事件");
        }
        let current = self.contract_report(module)?;
        let version = self.candidate_version(base)?;
        if let Some(artifact) = artifact {
            validate_artifact_version(artifact, &version)?;
        }
        if let Some(url) = artifact.and_then(|item| item.url.as_deref()) {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                bail!("制品地址必须是 http 或 https URL");
            }
        }
        if let Some(sha) = artifact.and_then(|item| item.sha256.as_deref()) {
            if sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!("制品 SHA-256 必须是 64 位十六进制字符串");
            }
        }
        if artifact.and_then(|item| item.sha256.as_deref()).is_some()
            && artifact.and_then(|item| item.url.as_deref()).is_none()
        {
            bail!("指定制品 SHA-256 时必须同时指定 --artifact-url");
        }
        let consumers = self.local_affected_consumer_names(provider)?;
        let declared = self.declared_consumer_names(provider)?;
        let targets =
            routing::candidate_targets(&metadata, &consumers, &declared, consumer_targets)?;
        let mut compatibility = serde_json::json!({
            "breaking": false,
            "removed": [],
            "added": [],
        });
        if let Some(path) = baseline {
            let baseline_report: serde_json::Value = read_json(Path::new(path))
                .with_context(|| format!("读取契约基线失败：{}", path))?;
            let before = symbols_from_report(&baseline_report)?;
            let after = symbols_from_report(&current)?;
            compatibility = serde_json::json!({
                "breaking": !before.is_subset(&after),
                "removed": before.difference(&after).collect::<Vec<_>>(),
                "added": after.difference(&before).collect::<Vec<_>>(),
                "baseline_contract_sha256": baseline_report.get("contract_sha256").cloned().unwrap_or(serde_json::Value::Null),
            });
        }
        let event_id = format!(
            "{}-{}",
            Utc::now().format("%Y%m%dT%H%M%S"),
            &unique_token()[..12]
        );
        let event = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "event_id": event_id,
            "event_type": "contract_candidate",
            "created_at": Utc::now().to_rfc3339(),
            "provider": provider,
            "candidate_version": version,
            "repository": metadata.repository,
            "worktree_path": metadata.worktree_path,
            "branch": metadata.branch,
            "commit": metadata.commit,
            "contract_sha256": current["contract_sha256"],
            "symbol_count": current["symbol_count"],
            "affected_consumers": targets.keys().collect::<Vec<_>>(),
            "routing_schema_version": 1,
            "consumer_targets": targets,
            "compatibility": compatibility,
            "artifact": {
                "url": artifact.and_then(|item| item.url.as_deref()),
                "sha256": artifact.and_then(|item| item.sha256.as_deref()),
                "coordinate": artifact.and_then(|item| item.coordinate.as_deref()),
            },
        });
        let event_id = event["event_id"].as_str().expect("event id");
        routing::validate_event_routes(&event)?;
        let events_dir = self.state_root.join("events");
        fs::create_dir_all(&events_dir)?;
        let path = events_dir.join(format!("{event_id}.json"));
        write_json_atomic(&path, &event)?;
        record_event(
            &self.state_root,
            "contract_candidate_created",
            event.clone(),
        )?;
        if json {
            println!("{}", serde_json::to_string_pretty(&event)?);
        } else {
            println!("候选事件已创建：{}", event_id);
            println!("候选版本：{}", event["candidate_version"]);
            println!(
                "受影响消费者：{}",
                targets.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        Ok(())
    }

    pub fn event_inbox(&self, consumer: &str, json: bool) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        let events_dir = self.state_root.join("events");
        let mut events = Vec::new();
        if let Ok(entries) = fs::read_dir(&events_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let event: serde_json::Value = match read_json(&path) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                if !event["affected_consumers"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(consumer)))
                {
                    continue;
                }
                let event_id = event["event_id"].as_str().unwrap_or_default();
                if !valid_event_id(event_id) {
                    continue;
                }
                if routing::require_consumer_repository(&event, consumer, &metadata.repository)
                    .is_err()
                {
                    continue;
                }
                let ack_path = events_dir
                    .join(event_id)
                    .join("acks")
                    .join(format!("{}.json", safe_component(consumer)));
                let ack = read_json::<serde_json::Value>(&ack_path).ok();
                let mut item = event;
                item["consumer"] = serde_json::json!(consumer);
                item["effective_status"] = serde_json::json!(
                    ack.as_ref()
                        .map(|ack| scoped_ack_status(ack, &item, consumer))
                        .unwrap_or("pending")
                );
                item["ack"] = ack.unwrap_or(serde_json::Value::Null);
                events.push(item);
            }
        }
        events.sort_by(|left, right| {
            left["created_at"]
                .as_str()
                .unwrap_or_default()
                .cmp(right["created_at"].as_str().unwrap_or_default())
        });
        if json {
            println!("{}", serde_json::to_string_pretty(&events)?);
        } else if events.is_empty() {
            println!("{} 没有待处理的候选事件", consumer);
        } else {
            for event in &events {
                let status = event["effective_status"].as_str().unwrap_or("pending");
                println!(
                    "{} {} {}",
                    event["event_id"], event["candidate_version"], status
                );
            }
        }
        Ok(())
    }

    pub fn import_event(&self, path: &str, json: bool) -> Result<()> {
        let source = PathBuf::from(path);
        let event: serde_json::Value = read_json(&source)
            .with_context(|| format!("读取候选事件文件失败：{}", source.display()))?;
        let event_id = event["event_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("候选事件缺少 event_id"))?;
        if !valid_event_id(event_id) {
            bail!("候选事件 ID 包含非法字符");
        }
        if event["event_type"].as_str() != Some("contract_candidate") {
            bail!("事件类型不是 contract_candidate，拒绝导入");
        }
        let version = event["candidate_version"].as_str().unwrap_or_default();
        if !fixed_version(version) {
            bail!("候选事件缺少有效的固定版本");
        }
        validate_artifact_version(
            &ArtifactEvidence {
                url: event["artifact"]["url"].as_str().map(str::to_string),
                sha256: event["artifact"]["sha256"].as_str().map(str::to_string),
                coordinate: event["artifact"]["coordinate"].as_str().map(str::to_string),
            },
            version,
        )?;
        if event["artifact"]["url"]
            .as_str()
            .is_some_and(|url| !(url.starts_with("https://") || url.starts_with("http://")))
        {
            bail!("候选事件制品地址必须是 http 或 https URL");
        }
        if event["artifact"]["sha256"]
            .as_str()
            .is_some_and(|sha| sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            bail!("候选事件制品 SHA-256 无效");
        }
        let consumers = event["affected_consumers"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("候选事件缺少 affected_consumers"))?;
        if consumers
            .iter()
            .any(|item| item.as_str().is_none_or(|value| !valid_agent_id(value)))
        {
            bail!("候选事件包含无效消费者名称");
        }
        let events_dir = self.state_root.join("events");
        fs::create_dir_all(&events_dir)?;
        let target = events_dir.join(format!("{event_id}.json"));
        routing::validate_event_routes(&event)?;
        let _guard = StateFileLock::acquire(&target.with_extension("import.lock"))?;
        if target.exists() {
            let existing: serde_json::Value = read_json(&target)
                .with_context(|| format!("读取已导入事件失败：{}", target.display()))?;
            if existing != event {
                bail!("事件 ID 已存在但内容不同，拒绝覆盖：{}", event_id);
            }
        }
        write_json_atomic(&target, &event)?;
        record_event(
            &self.state_root,
            "contract_candidate_imported",
            serde_json::json!({"event_id": event_id, "source": source}),
        )?;
        if json {
            println!("{}", serde_json::to_string_pretty(&event)?);
        } else {
            println!("候选事件已导入：{}", event_id);
        }
        Ok(())
    }

    pub fn acknowledge_event(
        &self,
        event_id: &str,
        consumer: &str,
        status: &str,
        message: Option<&str>,
        receipt_path: Option<&str>,
        json: bool,
    ) -> Result<()> {
        if !valid_event_id(event_id) {
            bail!("候选事件 ID 包含非法字符");
        }
        if !matches!(
            status,
            "received" | "validation_started" | "passed" | "failed"
        ) {
            bail!("事件确认状态必须是 received、validation_started、passed 或 failed");
        }
        if !valid_agent_id(consumer) {
            bail!("消费者名称只能包含 ASCII 字母、数字、下划线和连字符");
        }
        let metadata = self.ensure_metadata(false)?;
        let event_path = self
            .state_root
            .join("events")
            .join(format!("{event_id}.json"));
        let event: serde_json::Value = read_json(&event_path)
            .with_context(|| format!("读取候选事件失败：{}", event_path.display()))?;
        let target = routing::require_consumer_repository(&event, consumer, &metadata.repository)?;
        if !event["affected_consumers"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(consumer)))
        {
            bail!("消费者 {} 不在事件 {} 的受影响列表中", consumer, event_id);
        }
        let ack_path = self
            .state_root
            .join("events")
            .join(event_id)
            .join("acks")
            .join(format!("{}.json", safe_component(consumer)));
        fs::create_dir_all(ack_path.parent().expect("确认目录"))?;
        let _guard = StateFileLock::acquire(&ack_path.with_extension("lock"))?;
        let previous = read_json::<serde_json::Value>(&ack_path).ok();
        let now = Utc::now();
        if status == "validation_started" {
            require_clean_worktree(&self.cwd)?;
            if previous.as_ref().is_some_and(|value| {
                value["status"] == "validation_started"
                    && value["expires_at"]
                        .as_str()
                        .is_some_and(|expiry| !lock_expired_fields(expiry))
            }) {
                if previous.as_ref().is_some_and(|value| {
                    scoped_ack_status(value, &event, consumer) == "needs_revalidation"
                }) {
                    bail!(
                        "已有未过期验证的仓库归属不完整或不匹配；为避免覆盖正在运行的验证，请由原 Agent 结束验证或等待租约到期"
                    );
                }
                bail!("消费者已有未结束的验证；请由原 Agent 报告 failed 或等待验证租约过期");
            }
        } else if matches!(status, "passed" | "failed") {
            validate_attempt_owner(previous.as_ref(), &metadata)?;
        } else if previous
            .as_ref()
            .is_some_and(|value| value["status"] != "received")
        {
            bail!("received 不得覆盖已有验证状态");
        }
        let local_receipt_path = self
            .state_root
            .join("agents")
            .join(safe_component(&metadata.agent_id))
            .join(&metadata.runtime_id)
            .join("build-result.json");
        let receipt = receipt_path
            .map(PathBuf::from)
            .map(|path| (path.clone(), read_json::<BuildReceipt>(&path)))
            .map(|(path, value)| (path, value.ok()))
            .or_else(|| {
                Some((
                    local_receipt_path.clone(),
                    read_json::<BuildReceipt>(&local_receipt_path).ok(),
                ))
            });
        if status == "passed" {
            let Some(receipt) = receipt.as_ref() else {
                bail!(
                    "确认 passed 前必须存在成功的构建收据：{}",
                    local_receipt_path.display()
                );
            };
            let Some(receipt_value) = receipt.1.as_ref() else {
                bail!("无法读取构建收据：{}", receipt.0.display());
            };
            require_clean_worktree(&self.cwd)?;
            validate_candidate_receipt(
                receipt_value,
                &metadata,
                &event,
                previous.as_ref().expect("验证状态已检查"),
                consumer,
            )?;
        }
        let attempt_id = if status == "validation_started" {
            serde_json::json!(unique_token())
        } else {
            previous
                .as_ref()
                .map(|value| value["attempt_id"].clone())
                .unwrap_or_default()
        };
        let evidence = receipt.as_ref().and_then(|item| item.1.as_ref());
        let ack = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "event_id": event_id,
            "consumer": consumer,
            "status": status,
            "message": message.unwrap_or_default(),
            "updated_at": now.to_rfc3339(),
            "attempt_id": attempt_id,
            "agent_id": metadata.agent_id,
            "worktree_id": metadata.worktree_id,
            "commit": metadata.commit,
            "repository": metadata.repository,
            "consumer_project": target.project,
            "expires_at": if status == "validation_started" { serde_json::json!((now + Duration::hours(2)).to_rfc3339()) } else { previous.as_ref().map(|value| value["expires_at"].clone()).unwrap_or_default() },
            "validation_started_at": if status == "validation_started" { serde_json::json!(now.to_rfc3339()) } else { previous.as_ref().map(|value| value["validation_started_at"].clone()).unwrap_or_default() },
            "evidence_version": if status == "passed" { 2 } else { 0 },
            "receipt_snapshot": if status == "passed" { serde_json::to_value(evidence)? } else { serde_json::Value::Null },
            "build_receipt": receipt.as_ref().and_then(|item| item.1.as_ref().map(|_| item.0.display().to_string())),
        });
        write_json_atomic(&ack_path, &ack)?;
        record_event(
            &self.state_root,
            "contract_candidate_acknowledged",
            ack.clone(),
        )?;
        if json {
            println!("{}", serde_json::to_string_pretty(&ack)?);
        } else {
            println!(
                "事件 {} 的消费者 {} 状态已更新为 {}",
                event_id, consumer, status
            );
        }
        Ok(())
    }

    pub fn event_status(&self, event_id: Option<&str>, json: bool) -> Result<()> {
        if event_id.is_some_and(|value| !valid_event_id(value)) {
            bail!("候选事件 ID 包含非法字符");
        }
        let events_dir = self.state_root.join("events");
        let mut reports = Vec::new();
        if let Ok(entries) = fs::read_dir(&events_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let event: serde_json::Value = match read_json(&path) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                if event_id.is_some_and(|wanted| event["event_id"].as_str() != Some(wanted)) {
                    continue;
                }
                let id = event["event_id"].as_str().unwrap_or_default();
                if !valid_event_id(id) || event["event_type"] != "contract_candidate" {
                    bail!("事件文件身份无效：{}", path.display());
                }
                let routing_error = routing::validate_event_routes(&event)
                    .err()
                    .map(|error| error.to_string());
                let consumers = event["affected_consumers"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let mut statuses = serde_json::Map::new();
                let mut pending = Vec::new();
                for consumer in consumers.iter().filter_map(serde_json::Value::as_str) {
                    let ack_path = events_dir
                        .join(id)
                        .join("acks")
                        .join(format!("{}.json", safe_component(consumer)));
                    let ack = read_json::<serde_json::Value>(&ack_path).ok();
                    let status = ack
                        .as_ref()
                        .map(|ack| scoped_ack_status(ack, &event, consumer))
                        .unwrap_or("pending");
                    if status != "passed" {
                        pending.push(consumer.to_string());
                    }
                    statuses.insert(
                        consumer.to_string(),
                        serde_json::json!({
                            "status": status,
                            "ack": ack,
                        }),
                    );
                }
                let ready = pending.is_empty() && routing_error.is_none();
                reports.push(serde_json::json!({
                    "event": event,
                    "consumers": statuses,
                    "pending_consumers": pending,
                    "routing_error": routing_error,
                    "ready_for_integration": ready,
                }));
            }
        }
        reports.sort_by(|left, right| {
            left["event"]["created_at"]
                .as_str()
                .unwrap_or_default()
                .cmp(right["event"]["created_at"].as_str().unwrap_or_default())
        });
        if event_id.is_some() && reports.is_empty() {
            bail!("没有找到候选事件：{}", event_id.unwrap_or_default());
        }
        if json {
            println!("{}", serde_json::to_string_pretty(&reports)?);
        } else {
            for report in &reports {
                println!(
                    "{} {} 待验证：{} 集成就绪：{}",
                    report["event"]["event_id"],
                    report["event"]["candidate_version"],
                    report["pending_consumers"].as_array().map_or(0, Vec::len),
                    if report["ready_for_integration"].as_bool().unwrap_or(false) {
                        "是"
                    } else {
                        "否"
                    }
                );
            }
        }
        Ok(())
    }

    pub fn contract_snapshot(
        &self,
        json: bool,
        output: Option<&str>,
        module: Option<&str>,
    ) -> Result<()> {
        let report = self.contract_report(module)?;
        if let Some(output) = output {
            let path = self.resolve_contract_output(output)?;
            write_json_atomic(&path, &report)?;
        }
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!(
                "contract_sha256={} symbols={}",
                report["contract_sha256"], report["symbol_count"]
            );
        }
        Ok(())
    }

    fn contract_report(&self, module: Option<&str>) -> Result<serde_json::Value> {
        let metadata = self.ensure_metadata(false)?;
        let scan_root = self.contract_scan_root(module)?;
        let mut files = Vec::new();
        collect_files(&scan_root, &mut files)?;
        let mut symbols = Vec::new();
        for path in files {
            if path.extension().and_then(|ext| ext.to_str()) != Some("kt")
                || !path.components().any(|part| part.as_os_str() == "main")
                || !path.components().any(|part| part.as_os_str() == "src")
            {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap_or_default();
            let lines = text.lines().collect::<Vec<_>>();
            for (line_number, line) in lines.iter().enumerate() {
                let trimmed = line.trim();
                let public = trimmed.starts_with("public ")
                    || trimmed.starts_with("data class ")
                    || trimmed.starts_with("sealed class ")
                    || trimmed.starts_with("enum class ")
                    || trimmed.starts_with("interface ")
                    || trimmed.starts_with("class ")
                    || trimmed.starts_with("fun ");
                if public {
                    let mut signature = trimmed.to_string();
                    let mut paren_depth = trimmed.matches('(').count() as isize
                        - trimmed.matches(')').count() as isize;
                    let mut next = line_number + 1;
                    while paren_depth > 0 && next < lines.len() {
                        let next_line = lines[next].trim();
                        signature.push(' ');
                        signature.push_str(next_line);
                        paren_depth += next_line.matches('(').count() as isize;
                        paren_depth -= next_line.matches(')').count() as isize;
                        next += 1;
                    }
                    let declaration = signature
                        .split_once('{')
                        .map_or(signature.as_str(), |(head, _)| head)
                        .trim();
                    symbols.push(format!(
                        "{}:{}",
                        path.strip_prefix(&scan_root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .replace('\\', "/"),
                        declaration
                    ));
                }
            }
        }
        symbols.sort();
        let canonical = symbols.join("\n");
        let digest = Sha256::digest(canonical.as_bytes());
        let sha = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let report = serde_json::json!({
            "snapshot_format": "kotlin-production-declarations-v2",
            "repository": metadata.repository,
            "commit": metadata.commit,
            "contract_sha256": sha,
            "symbol_count": symbols.len(),
            "symbols": symbols,
        });
        Ok(report)
    }

    fn contract_scan_root(&self, module: Option<&str>) -> Result<PathBuf> {
        let Some(module) = module else {
            return Ok(self.cwd.clone());
        };
        let path = PathBuf::from(module);
        if path.is_absolute()
            || path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        {
            bail!("契约模块路径必须是当前 Worktree 下的相对路径：{}", module);
        }
        let root = self.cwd.join(path);
        if !root.is_dir() {
            bail!("契约模块目录不存在：{}", root.display());
        }
        Ok(root)
    }

    fn resolve_contract_output(&self, value: &str) -> Result<PathBuf> {
        let path = PathBuf::from(value);
        if !path.is_absolute()
            || path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        {
            bail!("契约输出文件必须是无上级跳转的绝对路径：{}", path.display());
        }
        let worktree = fs::canonicalize(&self.cwd).context("规范化 Worktree 路径失败")?;
        let state_root =
            fs::canonicalize(&self.state_root).context("规范化运行时状态根目录失败")?;
        let identity = path_identity(&path);
        let worktree_identity = path_identity(&worktree);
        let state_identity = path_identity(&state_root);
        if !identity.starts_with(&format!("{worktree_identity}\\"))
            && !identity.starts_with(&format!("{state_identity}\\"))
        {
            bail!(
                "契约输出文件必须位于当前 Worktree 或运行时状态目录：{}",
                path.display()
            );
        }
        Ok(path)
    }

    pub fn contract_diff(
        &self,
        baseline: &str,
        module: Option<&str>,
        json: bool,
        enforce: bool,
    ) -> Result<()> {
        let baseline_path = PathBuf::from(baseline);
        if !baseline_path.is_absolute() {
            bail!("契约基线必须使用绝对路径：{}", baseline_path.display());
        }
        let baseline_report: serde_json::Value = read_json(&baseline_path)
            .with_context(|| format!("读取契约基线失败：{}", baseline_path.display()))?;
        let current = self.contract_report(module)?;
        let before = symbols_from_report(&baseline_report)?;
        let after = symbols_from_report(&current)?;
        let removed = before.difference(&after).cloned().collect::<Vec<_>>();
        let added = after.difference(&before).cloned().collect::<Vec<_>>();
        let report = serde_json::json!({
            "baseline": baseline_path,
            "baseline_contract_sha256": baseline_report.get("contract_sha256").cloned().unwrap_or(serde_json::Value::Null),
            "current_contract_sha256": current["contract_sha256"],
            "removed": removed,
            "added": added,
            "breaking": !removed.is_empty(),
        });
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else if report["breaking"].as_bool().unwrap_or(false) {
            println!(
                "契约存在破坏性变更：删除 {} 个公开符号，新增 {} 个公开符号",
                report["removed"].as_array().map_or(0, Vec::len),
                report["added"].as_array().map_or(0, Vec::len)
            );
            for item in report["removed"].as_array().into_iter().flatten() {
                println!("删除：{}", item);
            }
        } else {
            println!(
                "契约兼容：新增 {} 个公开符号，未删除公开符号",
                report["added"].as_array().map_or(0, Vec::len)
            );
        }
        if enforce && report["breaking"].as_bool().unwrap_or(false) {
            bail!("契约差异包含破坏性变更");
        }
        Ok(())
    }

    pub fn lock_build(&self, timeout: u64, lease: u64, owner: Option<&str>) -> Result<()> {
        if lease == 0 {
            bail!("构建锁租期必须大于零");
        }
        let metadata = self.ensure_metadata(false)?;
        if owner.is_some_and(|owner| owner != metadata.agent_id) {
            bail!("--owner 必须与当前 Agent ID 一致");
        }
        let token = acquire_lock_internal(self, timeout, lease, owner)?;
        write_text_atomic(
            &PathBuf::from(&metadata.paths.root).join("locks/build.token"),
            &token,
        )?;
        println!("当前 Worktree 的构建锁已获取。构建完成后运行 agentctl unlock-build。");
        Ok(())
    }

    pub fn unlock_build(&self, token: Option<&str>) -> Result<()> {
        self.release_build_lock(token, false)
    }

    pub fn acquire_device(
        &self,
        requested: Option<&str>,
        wait: u64,
        lease_seconds: u64,
        json: bool,
    ) -> Result<()> {
        if lease_seconds == 0 {
            bail!("设备租期必须大于零");
        }
        let metadata = self.ensure_metadata(false)?;
        if let Ok(existing) = read_json::<TestLease>(
            &PathBuf::from(&metadata.paths.root).join("locks/device-operation.json"),
        ) {
            if process_identity_alive(existing.pid, &existing.process_start_time) {
                bail!(
                    "当前 Worktree 正在通过 PID {} 操作设备 {}",
                    existing.pid,
                    existing.serial
                );
            }
        }
        let started = Instant::now();
        loop {
            let (acquired, last_error) = {
                let worktree_gate =
                    PathBuf::from(&metadata.paths.worktree_root).join("locks/build-gate.lock");
                let _worktree_guard = StateFileLock::acquire(&worktree_gate)?;
                let runtime_gate =
                    PathBuf::from(&metadata.paths.root).join("locks/device-acquire.lock");
                let _runtime_guard = StateFileLock::acquire(&runtime_gate)?;
                let worktree_record: serde_json::Value =
                    read_json(&PathBuf::from(&metadata.paths.worktree_root).join("worktree.json"))
                        .context("读取 Worktree runtime 状态失败")?;
                if worktree_record
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    != Some("active")
                {
                    bail!("Worktree runtime 已清理或未激活，拒绝申请设备");
                }
                if let Ok(operation) = read_json::<TestLease>(
                    &PathBuf::from(&metadata.paths.root).join("locks/device-operation.json"),
                ) {
                    if process_identity_alive(operation.pid, &operation.process_start_time) {
                        bail!(
                            "当前 Worktree 正在通过 PID {} 操作设备 {}",
                            operation.pid,
                            operation.serial
                        );
                    }
                }
                let devices = available_devices()?;
                let mut last_error = None;
                let serials = match requested {
                    Some(serial) => {
                        if !devices.iter().any(|item| item == serial) {
                            bail!("设备 {} 未连接或 ADB 状态不是 device", serial);
                        }
                        vec![serial.to_string()]
                    }
                    None => devices.clone(),
                };
                let mut acquired = None;
                for serial in serials {
                    match acquire_device_lease_from_devices_locked(
                        self,
                        &metadata,
                        &serial,
                        lease_seconds,
                        &devices,
                    ) {
                        Ok(lease) => {
                            acquired = Some(lease);
                            break;
                        }
                        Err(error) => last_error = Some(error),
                    }
                }
                (acquired, last_error)
            };
            if let Some(lease) = acquired {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "agent_id": lease.agent_id,
                            "serial": lease.serial,
                            "acquired_at": lease.acquired_at,
                            "expires_at": lease.expires_at,
                            "last_heartbeat": lease.heartbeat_at,
                            "status": lease.status,
                        }))?
                    );
                } else {
                    println!(
                        "设备租约已获取：{}；在 Codex 构建环境中设置 ANDROID_SERIAL='{}'",
                        lease.serial, lease.serial
                    );
                }
                return Ok(());
            }
            if requested.is_some() && wait == 0 {
                return Err(last_error.unwrap_or_else(|| anyhow::anyhow!("指定设备不可用")));
            }
            if started.elapsed().as_secs() >= wait {
                return Err(
                    last_error.unwrap_or_else(|| anyhow::anyhow!("没有发现可用 Android 设备"))
                )
                .context("等待设备租约超时");
            }
            thread::sleep(StdDuration::from_secs(1));
        }
    }

    pub fn release_device(&self, token: Option<&str>) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        let worktree_gate =
            PathBuf::from(&metadata.paths.worktree_root).join("locks/build-gate.lock");
        let _worktree_guard = StateFileLock::acquire(&worktree_gate)?;
        let runtime_gate = PathBuf::from(&metadata.paths.root).join("locks/device-acquire.lock");
        let _runtime_guard = StateFileLock::acquire(&runtime_gate)?;
        if let Ok(active) = read_json::<TestLease>(
            &PathBuf::from(&metadata.paths.root).join("locks/device-operation.json"),
        ) {
            if process_identity_alive(active.pid, &active.process_start_time) {
                bail!("设备正在由 PID {} 操作，完成后才能释放租约", active.pid);
            }
        }
        let path = PathBuf::from(&metadata.paths.root).join("device-lease.json");
        let lease = read_json::<DeviceLease>(&path).context("当前 Worktree 没有有效设备租约")?;
        let supplied = token
            .map(str::to_string)
            .or_else(|| env::var("AGENT_DEVICE_LEASE_TOKEN").ok())
            .or_else(|| {
                fs::read_to_string(PathBuf::from(&metadata.paths.root).join("device-lease.token"))
                    .ok()
            })
            .ok_or_else(|| {
                anyhow::anyhow!("释放设备租约需要 --token 或 AGENT_DEVICE_LEASE_TOKEN")
            })?;
        if lease.token != supplied
            || lease.runtime_id != metadata.runtime_id
            || lease.agent_id != metadata.agent_id
        {
            bail!("设备租约令牌或运行时身份不匹配");
        }
        remove_device_lease(&self.state_root, &lease)?;
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(PathBuf::from(&metadata.paths.root).join("device-lease.token"));
        record_event(
            &self.state_root,
            "device_lease_released",
            serde_json::json!({"runtime_id": lease.runtime_id, "serial": lease.serial, "owner": lease.agent_id}),
        )?;
        println!("设备租约已释放：{}", lease.serial);
        Ok(())
    }

    pub fn heartbeat_device(&self) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        let path = PathBuf::from(&metadata.paths.root).join("device-lease.json");
        let lease = read_json::<DeviceLease>(&path).context("当前 Worktree 没有设备租约")?;
        let token = env::var("AGENT_DEVICE_LEASE_TOKEN")
            .ok()
            .or_else(|| {
                fs::read_to_string(PathBuf::from(&metadata.paths.root).join("device-lease.token"))
                    .ok()
            })
            .ok_or_else(|| anyhow::anyhow!("找不到设备租约令牌"))?;
        if token != lease.token || lease.runtime_id != metadata.runtime_id {
            bail!("设备租约不属于当前 Worktree");
        }
        renew_device_lease(&self.state_root, &metadata, &lease)?;
        record_event(
            &self.state_root,
            "device_lease_heartbeat",
            serde_json::json!({"runtime_id": lease.runtime_id, "serial": lease.serial}),
        )?;
        println!("设备租约已续期：{}", lease.serial);
        Ok(())
    }

    pub fn allocate_port(
        &self,
        name: &str,
        preferred: Option<u16>,
        base: u16,
        range: u16,
    ) -> Result<()> {
        if range == 0 {
            bail!("端口分配范围必须大于零");
        }
        if u32::from(base) + u32::from(range) - 1 > u32::from(u16::MAX) {
            bail!("端口分配范围超出有效端口上限");
        }
        let metadata = self.ensure_metadata(false)?;
        let registry_path = self.state_root.join("ports.json");
        fs::create_dir_all(&self.state_root)?;
        let registry_lock = registry_path.with_extension("lock");
        let _guard = StateFileLock::acquire(&registry_lock)?;
        let mut registry = load_port_registry(&registry_path)?;
        registry.retain(|entry| {
            !port_expired(entry) && worktree_is_active(&self.state_root, &entry.runtime_id)
        });
        for entry in registry.iter_mut().filter(|entry| {
            entry.runtime_id == metadata.runtime_id && entry.owner == metadata.agent_id
        }) {
            entry.expires_at = (Utc::now() + Duration::days(7)).to_rfc3339();
            entry.status = "active".to_string();
        }
        if let Some(existing) = registry.iter().find(|entry| {
            entry.runtime_id == metadata.runtime_id
                && entry.owner == metadata.agent_id
                && entry.name == name
        }) {
            let port = existing.port;
            drop(_guard);
            refresh_port_leases(&registry_path, &metadata)?;
            println!("{}={}", name, port);
            return Ok(());
        }
        let digest = Sha256::digest(format!("{}:{}", metadata.runtime_id, name).as_bytes());
        let seed = u16::from_le_bytes([digest[0], digest[1]]) % range;
        let mut selected = None;
        if let Some(port) = preferred {
            let end = u32::from(base) + u32::from(range);
            if u32::from(port) >= u32::from(base)
                && u32::from(port) < end
                && !registry.iter().any(|entry| entry.port == port)
                && !is_tcp_port_bound(port)
            {
                selected = Some(port);
            }
        }
        for offset in 0..range {
            if selected.is_some() {
                break;
            }
            let candidate = base + (seed + offset) % range;
            if candidate == 0
                || registry.iter().any(|entry| entry.port == candidate)
                || is_tcp_port_bound(candidate)
            {
                continue;
            }
            selected = Some(candidate);
            break;
        }
        let port = selected.ok_or_else(|| {
            anyhow::anyhow!(
                "端口范围 {}..={} 已耗尽",
                base,
                base.saturating_add(range - 1)
            )
        })?;
        let runtime_id = metadata.runtime_id.clone();
        let owner = metadata.agent_id.clone();
        registry.push(PortLease {
            schema_version: SCHEMA_VERSION,
            runtime_id: runtime_id.clone(),
            owner: owner.clone(),
            name: name.to_string(),
            port,
            created_at: Utc::now().to_rfc3339(),
            expires_at: (Utc::now() + Duration::days(7)).to_rfc3339(),
            status: "active".to_string(),
        });
        write_json_atomic(&registry_path, &registry)?;
        let local = registry
            .iter()
            .filter(|entry| entry.runtime_id == runtime_id && entry.owner == owner)
            .cloned()
            .collect::<Vec<_>>();
        write_json_atomic(
            &PathBuf::from(&metadata.paths.root).join("ports.json"),
            &local,
        )?;
        record_event(
            &self.state_root,
            "port_allocated",
            serde_json::json!({"runtime_id": runtime_id, "name": name, "port": port, "owner": owner}),
        )?;
        println!("{}={}", name, port);
        Ok(())
    }

    pub fn list_ports(&self, json: bool) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        let path = self.state_root.join("ports.json");
        let _guard = StateFileLock::acquire(&path.with_extension("lock"))?;
        let entries = load_port_registry(&path)?
            .into_iter()
            .filter(|entry| {
                entry.runtime_id == metadata.runtime_id
                    && entry.owner == metadata.agent_id
                    && !port_expired(entry)
            })
            .collect::<Vec<_>>();
        if json {
            println!("{}", serde_json::to_string_pretty(&entries)?);
        } else {
            for entry in entries {
                println!(
                    "{}={} owner={} runtime={}",
                    entry.name, entry.port, entry.owner, entry.runtime_id
                );
            }
        }
        Ok(())
    }

    pub fn run_android_test(&self, args: &[String]) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        let path = PathBuf::from(&metadata.paths.root).join("device-lease.json");
        let lease =
            read_json::<DeviceLease>(&path).context("运行 Android 测试前必须先获取设备租约")?;
        let supplied = env::var("AGENT_DEVICE_LEASE_TOKEN")
            .ok()
            .or_else(|| {
                fs::read_to_string(PathBuf::from(&metadata.paths.root).join("device-lease.token"))
                    .ok()
            })
            .ok_or_else(|| anyhow::anyhow!("未找到设备租约令牌；先运行 acquire-device"))?;
        if lease.token != supplied
            || lease.runtime_id != metadata.runtime_id
            || lease.agent_id != metadata.agent_id
            || lease.status != "active"
        {
            bail!("设备租约不属于当前 Agent/Worktree 或已失效");
        }
        if lock_expired_fields(&lease.expires_at) {
            bail!("设备租约已过期，请重新获取");
        }
        validate_device_lease_global(&self.state_root, &lease)?;
        validate_gradle_args(args, false)?;
        if args.is_empty() {
            bail!("缺少 Android 测试命令，例如 connectedAndroidTest");
        }
        if args.iter().any(|arg| {
            arg == "-s"
                || arg == "--serial"
                || arg.starts_with("-s=")
                || arg.starts_with("--serial=")
        }) {
            bail!("禁止在测试参数中覆盖设备序列号；租约会注入当前 ANDROID_SERIAL");
        }
        let wrapper = if cfg!(windows) {
            self.cwd.join("gradlew.bat")
        } else {
            self.cwd.join("gradlew")
        };
        if !wrapper.is_file() {
            bail!("当前 Worktree 未找到 Gradle Wrapper：{}", wrapper.display());
        }
        let workers = env::var("AGENT_MAX_WORKERS").ok();
        if let Some(value) = workers.as_deref() {
            if value
                .parse::<u32>()
                .ok()
                .filter(|count| *count > 0)
                .is_none()
            {
                bail!("AGENT_MAX_WORKERS 必须是正整数");
            }
        }
        let test_args = android_test_gradle_args(args);
        let gradle_args = self.gradle_args(&metadata, &test_args, workers.as_deref())?;
        let token = lease.token.clone();
        let build_token = acquire_lock_internal(self, 0, 7200, None)?;
        let test_token = match acquire_device_operation(self, &metadata, &lease) {
            Ok(token) => token,
            Err(error) => {
                let _ = self.release_build_lock(Some(&build_token), true);
                return Err(error);
            }
        };
        let keep_alive = Arc::new(AtomicBool::new(true));
        let renewal_failed = Arc::new(AtomicBool::new(false));
        let renewal_thread = spawn_lease_heartbeat(
            self.state_root.clone(),
            metadata.clone(),
            lease.clone(),
            keep_alive.clone(),
            renewal_failed.clone(),
        );
        let worktree_lock_path =
            PathBuf::from(&metadata.paths.worktree_root).join("locks/build.json");
        let output_result = if cfg!(windows) {
            let command_result = windows_gradle_command(&wrapper, &gradle_args);
            command_result.and_then(|mut command| {
                command
                    .current_dir(&self.cwd)
                    .env("GRADLE_USER_HOME", &metadata.paths.gradle_user_home)
                    .env("ANDROID_SERIAL", &lease.serial)
                    .env("MAVEN_REPO_LOCAL", &metadata.paths.maven_local)
                    .env("AGENT_DEVICE_LEASE_TOKEN", &token);
                run_managed_output(&mut command, &worktree_lock_path, &build_token)
            })
        } else {
            let mut command = Command::new(&wrapper);
            command
                .args(&gradle_args)
                .current_dir(&self.cwd)
                .env("GRADLE_USER_HOME", &metadata.paths.gradle_user_home)
                .env("ANDROID_SERIAL", &lease.serial)
                .env("MAVEN_REPO_LOCAL", &metadata.paths.maven_local)
                .env("AGENT_DEVICE_LEASE_TOKEN", &token);
            run_managed_output(&mut command, &worktree_lock_path, &build_token)
        }
        .context("启动 Android 测试失败");
        keep_alive.store(false, Ordering::SeqCst);
        let _ = renewal_thread.join();
        let test_release_result = release_device_operation(self, &metadata, &test_token);
        let release_result = self.release_build_lock(Some(&build_token), true);
        let operation_error = test_release_result.err();
        let build_error = release_result.err();
        let output = match output_result {
            Ok(output) => {
                if let Some(error) = operation_error {
                    return Err(error).context("Android 测试结束后释放设备操作锁失败");
                }
                if let Some(error) = build_error {
                    return Err(error).context("Android 测试结束后释放构建锁失败");
                }
                output
            }
            Err(error) => {
                let lease_error = self.release_device(Some(&token)).err();
                if let Some(operation_error) = operation_error {
                    let mut detail = format!("另外设备操作锁释放失败：{operation_error:#}");
                    if let Some(lease_error) = lease_error {
                        detail.push_str(&format!("；设备租约释放失败：{lease_error:#}"));
                    }
                    return Err(error.context(detail));
                }
                if let Some(build_error) = build_error {
                    let detail = if let Some(lease_error) = lease_error {
                        format!(
                            "另外构建锁释放失败：{build_error:#}；设备租约释放失败：{lease_error:#}"
                        )
                    } else {
                        format!("另外构建锁释放失败：{build_error:#}")
                    };
                    return Err(error.context(detail));
                }
                if let Some(lease_error) = lease_error {
                    return Err(error.context(format!(
                        "Android 测试失败后释放设备租约失败：{lease_error:#}"
                    )));
                }
                return Err(error);
            }
        };
        if renewal_failed.load(Ordering::SeqCst) {
            let _ = self.release_device(Some(&token));
            bail!("Android 测试期间设备租约续期失败，结果不可信");
        }
        let log_path = PathBuf::from(&metadata.paths.logs).join(format!(
            "android-test-{}.log",
            Utc::now().format("%Y%m%dT%H%M%SZ")
        ));
        let mut log = File::create(&log_path)?;
        log.write_all(&output.stdout)?;
        log.write_all(&output.stderr)?;
        if !output.status.success() {
            self.release_device(Some(&token))
                .context("Android 测试失败后释放设备租约失败")?;
            bail!(
                "Android 测试失败，退出码 {:?}；日志：{}",
                output.status.code(),
                log_path.display()
            );
        }
        println!(
            "Android 测试通过，设备 {}；日志：{}",
            lease.serial,
            log_path.display()
        );
        Ok(())
    }

    pub fn run_adb(&self, args: &[String]) -> Result<()> {
        self.run_adb_with_command(Command::new("adb"), args)
    }

    fn run_adb_with_command(&self, mut command: Command, args: &[String]) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        validate_adb_args(args)?;
        let lease_path = PathBuf::from(&metadata.paths.root).join("device-lease.json");
        let lease =
            read_json::<DeviceLease>(&lease_path).context("运行 ADB 命令前必须先获取设备租约")?;
        let token = env::var("AGENT_DEVICE_LEASE_TOKEN")
            .ok()
            .or_else(|| {
                fs::read_to_string(PathBuf::from(&metadata.paths.root).join("device-lease.token"))
                    .ok()
            })
            .ok_or_else(|| anyhow::anyhow!("未找到设备租约令牌；先运行 acquire-device"))?;
        if lease.token != token
            || lease.runtime_id != metadata.runtime_id
            || lease.agent_id != metadata.agent_id
            || lease.status != "active"
            || lock_expired_fields(&lease.expires_at)
        {
            bail!("设备租约不属于当前 Agent/Worktree 或已失效");
        }
        validate_device_lease_global(&self.state_root, &lease)?;
        let build_token = acquire_lock_internal(self, 0, 7200, None)?;
        let operation_token = match acquire_device_operation(self, &metadata, &lease) {
            Ok(token) => token,
            Err(error) => {
                let _ = self.release_build_lock(Some(&build_token), true);
                return Err(error);
            }
        };
        let build_lock_path = PathBuf::from(&metadata.paths.worktree_root).join("locks/build.json");
        let keep_alive = Arc::new(AtomicBool::new(true));
        let renewal_failed = Arc::new(AtomicBool::new(false));
        let renewal_thread = spawn_lease_heartbeat(
            self.state_root.clone(),
            metadata.clone(),
            lease.clone(),
            keep_alive.clone(),
            renewal_failed.clone(),
        );
        let log_base = PathBuf::from(&metadata.paths.logs)
            .join(format!("adb-{}", Utc::now().format("%Y%m%dT%H%M%SZ")));
        command
            .arg("-s")
            .arg(&lease.serial)
            .args(args)
            .current_dir(&self.cwd)
            .env("ANDROID_SERIAL", &lease.serial)
            .env("AGENT_DEVICE_LEASE_TOKEN", &token);
        let output_result = run_managed_streaming(
            &mut command,
            &build_lock_path,
            &build_token,
            &log_base,
            false,
        )
        .context("启动受租约保护的 ADB 命令失败");
        keep_alive.store(false, Ordering::SeqCst);
        let _ = renewal_thread.join();
        let release_result = release_device_operation(self, &metadata, &operation_token);
        let build_release_result = self.release_build_lock(Some(&build_token), true);
        let status = match (output_result, release_result, build_release_result) {
            (Ok(status), Ok(()), Ok(())) => status,
            (Err(error), Ok(()), Ok(())) => {
                self.release_device(Some(&token))
                    .context("ADB 命令启动失败后释放设备租约失败")?;
                return Err(error);
            }
            (Ok(_), Err(error), Ok(())) => {
                return Err(error).context("ADB 命令结束后释放设备操作锁失败");
            }
            (Ok(_), Ok(()), Err(error)) => {
                return Err(error).context("ADB 命令结束后释放构建锁失败");
            }
            (Err(error), Err(release_error), _) => {
                return Err(error.context(format!("另外设备操作锁释放失败：{release_error:#}")));
            }
            (Err(error), Ok(()), Err(build_error)) => {
                return Err(error.context(format!("另外构建锁释放失败：{build_error:#}")));
            }
            (Ok(_), Err(release_error), Err(build_error)) => {
                return Err(release_error.context(format!("另外构建锁释放失败：{build_error:#}")));
            }
        };
        if renewal_failed.load(Ordering::SeqCst) {
            self.release_device(Some(&token))
                .context("ADB 命令续租失败后释放设备租约失败")?;
            bail!(
                "ADB 命令期间设备租约续期失败，结果不可信；日志：{}.*.log",
                log_base.display()
            );
        }
        if !status.success() {
            self.release_device(Some(&token))
                .context("ADB 命令失败后释放设备租约失败")?;
            bail!(
                "ADB 命令失败，退出码 {:?}；日志：{}",
                status.code(),
                log_base.display()
            );
        }
        println!(
            "ADB 命令通过，设备 {}；日志前缀：{}",
            lease.serial,
            log_base.display()
        );
        Ok(())
    }

    fn release_build_lock(&self, token: Option<&str>, allow_current_process: bool) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        let lock_path = PathBuf::from(&metadata.paths.worktree_root).join("locks/build.json");
        let lock = match read_json::<BuildLock>(&lock_path) {
            Ok(lock) => lock,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                eprintln!("当前 Worktree 没有构建锁");
                return Ok(());
            }
            Err(error) => return Err(error).context("读取构建锁失败；不会删除未知状态"),
        };
        let supplied = token
            .map(str::to_string)
            .or_else(|| env::var("AGENT_BUILD_LOCK_TOKEN").ok())
            .or_else(|| {
                fs::read_to_string(PathBuf::from(&metadata.paths.root).join("locks/build.token"))
                    .ok()
            })
            .ok_or_else(|| {
                anyhow::anyhow!("释放锁需要提供获取锁时返回的 --token 或 AGENT_BUILD_LOCK_TOKEN")
            })?;
        if supplied != lock.token {
            bail!("构建锁令牌不匹配，拒绝释放");
        }
        if lock.runtime_id != metadata.runtime_id {
            bail!("构建锁运行时身份不匹配，拒绝释放");
        }
        if lock.owner != metadata.agent_id {
            bail!(
                "构建锁由 Agent {} 持有，当前 Agent {} 不得释放",
                lock.owner,
                metadata.agent_id
            );
        }
        if lock.pid != std::process::id() && process_matches(&lock) {
            bail!(
                "构建锁仍由其他运行中的 PID {} 持有；等待租约过期后由 doctor 回收",
                lock.pid
            );
        }
        if lock.pid == std::process::id() && !allow_current_process && process_matches(&lock) {
            bail!("锁由当前进程持有；只能由 run-gradle 完成后释放");
        }
        remove_lock_if_unchanged(&lock_path, &lock)?;
        let _ = fs::remove_file(PathBuf::from(&metadata.paths.root).join("locks/build.token"));
        eprintln!("构建锁已释放");
        Ok(())
    }

    pub fn run_gradle(
        &self,
        args: &[String],
        allow_clean: bool,
        allow_primary: bool,
        event_id: Option<&str>,
    ) -> Result<()> {
        let metadata = self.ensure_metadata(allow_primary)?;
        if metadata.is_primary && !allow_primary {
            bail!("主 Worktree 构建需要显式指定 --allow-primary");
        }
        validate_gradle_args(args, allow_clean)?;
        if args.iter().any(|argument| {
            argument.starts_with("--max-workers")
                || argument.starts_with("-Dmaven.repo.local")
                || argument.starts_with("-Dorg.gradle.jvmargs")
        }) {
            bail!("worker 数、Maven 仓库路径和 JVM 堆参数由运行时管理，不能通过任务参数覆盖");
        }
        if args
            .iter()
            .any(|argument| argument == "publishToMavenLocal")
            && env::var("AGENT_ALLOW_LOCAL_PUBLISH").ok().as_deref() != Some("1")
        {
            bail!(
                "默认禁止 publishToMavenLocal；跨仓候选制品必须发布唯一远端坐标。仅限当前 Worktree 临时联调时设置 AGENT_ALLOW_LOCAL_PUBLISH=1"
            );
        }
        let wrapper = if cfg!(windows) {
            self.cwd.join("gradlew.bat")
        } else {
            self.cwd.join("gradlew")
        };
        if !wrapper.is_file() {
            bail!("当前 Worktree 未找到 Gradle Wrapper：{}", wrapper.display());
        }
        let workers = env::var("AGENT_MAX_WORKERS").ok();
        if let Some(value) = workers.as_deref() {
            if value
                .parse::<u32>()
                .ok()
                .filter(|count| *count > 0)
                .is_none()
            {
                bail!("AGENT_MAX_WORKERS 必须是正整数");
            }
        }
        let token = acquire_lock_internal(self, 0, 7200, None)?;
        let result = self.run_gradle_locked(
            &metadata,
            &wrapper,
            args,
            workers.as_deref(),
            event_id,
            false,
        );
        let release_result = self.release_build_lock(Some(&token), true);
        match (result, release_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error).context("构建成功，但锁释放失败"),
            (Err(error), Err(release)) => {
                Err(error.context(format!("另外锁释放失败：{release:#}")))
            }
        }
    }

    pub fn install_cache(&self, snapshot: &str) -> Result<()> {
        let metadata = self.ensure_metadata(false)?;
        let snapshot = fs::canonicalize(snapshot).context("缓存快照目录不存在")?;
        let manifest_path = snapshot.join("manifest.json");
        let manifest: serde_json::Value = read_json(&manifest_path)?;
        if manifest["schema_version"].as_u64() != Some(1) {
            bail!("不支持的缓存快照格式");
        }
        if manifest["coverage"].as_str().is_none() {
            bail!("缓存快照缺少覆盖范围说明");
        }
        let expected_hash = manifest["wrapper_properties_sha256"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("快照缺少 Wrapper 配置摘要"))?;
        let wrapper_properties = self.cwd.join("gradle/wrapper/gradle-wrapper.properties");
        let actual_hash: String = Sha256::digest(fs::read(&wrapper_properties)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if actual_hash != expected_hash {
            bail!("当前 Worktree 的 Wrapper 配置与快照不一致");
        }

        let token = acquire_lock_internal(self, 0, 7200, None)?;
        let result = (|| -> Result<()> {
            let home = PathBuf::from(&metadata.paths.gradle_user_home);
            if snapshot.starts_with(&home) || home.starts_with(&snapshot) {
                bail!("缓存快照与目标 Gradle 用户目录不能互相包含");
            }
            let mut pairs = vec![
                (
                    snapshot.join("gradle-dependencies/modules-2"),
                    home.join("caches/modules-2"),
                ),
                (
                    snapshot.join("gradle-distribution"),
                    home.join("wrapper/dists"),
                ),
            ];
            let maven_test_runtime = snapshot.join("maven-test-runtime");
            if maven_test_runtime.is_dir() {
                pairs.push((
                    maven_test_runtime,
                    PathBuf::from(&metadata.paths.maven_local),
                ));
            }
            for (source, target) in &pairs {
                if !source.is_dir() {
                    bail!("缓存快照缺少分类目录：{}", source.display());
                }
                self.assert_inside_state_root(target)?;
                if target.is_symlink() {
                    bail!("目标缓存是符号链接，拒绝导入：{}", target.display());
                }
                if target.is_dir() && target.read_dir()?.next().is_some() {
                    bail!("目标缓存已有数据，拒绝覆盖：{}", target.display());
                }
            }
            for (source, target) in &pairs {
                copy_cache_tree(source, target)?;
            }
            Ok(())
        })();
        let release = self.release_build_lock(Some(&token), true);
        match (result, release) {
            (Ok(()), Ok(())) => {
                println!("缓存快照已导入当前 Worktree 的私有 Gradle 与测试 Maven 目录");
                Ok(())
            }
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error).context("导入完成但构建锁释放失败"),
            (Err(error), Err(release)) => {
                Err(error.context(format!("构建锁释放也失败：{release:#}")))
            }
        }
    }

    fn run_gradle_locked(
        &self,
        metadata: &Metadata,
        wrapper: &Path,
        args: &[String],
        workers: Option<&str>,
        event_id: Option<&str>,
        stdout_to_stderr: bool,
    ) -> Result<()> {
        let started = Instant::now();
        let started_at = Utc::now().to_rfc3339();
        let run_id = unique_token();
        let evidence_path = PathBuf::from(&metadata.paths.root)
            .join("validations")
            .join(format!("{run_id}.json"));
        fs::create_dir_all(evidence_path.parent().expect("验证目录"))?;
        let clean_before = require_clean_worktree(&self.cwd).is_ok();
        let log_base = PathBuf::from(&metadata.paths.logs).join(format!(
            "gradle-{}-{}",
            std::process::id(),
            Utc::now().format("%Y%m%dT%H%M%SZ")
        ));
        let gradle_args = self.gradle_args(metadata, args, workers)?;
        let build_lock_path = PathBuf::from(&metadata.paths.worktree_root).join("locks/build.json");
        let build_lock = read_json::<BuildLock>(&build_lock_path).context("读取当前构建锁失败")?;
        let status = if cfg!(windows) {
            let mut command = windows_gradle_command(wrapper, &gradle_args)?;
            command
                .current_dir(&self.cwd)
                .env("GRADLE_USER_HOME", &metadata.paths.gradle_user_home)
                .env("MAVEN_REPO_LOCAL", &metadata.paths.maven_local)
                .env("AGENT_VALIDATION_RUN_ID", &run_id)
                .env("AGENT_VALIDATION_EVENT_ID", event_id.unwrap_or_default())
                .env("AGENT_VALIDATION_EVIDENCE", &evidence_path);
            command.env_remove("GRADLE_RO_DEP_CACHE");
            run_managed_streaming(
                &mut command,
                &build_lock_path,
                &build_lock.token,
                &log_base,
                stdout_to_stderr,
            )
        } else {
            let mut command = Command::new(wrapper);
            command
                .args(&gradle_args)
                .current_dir(&self.cwd)
                .env("GRADLE_USER_HOME", &metadata.paths.gradle_user_home)
                .env("MAVEN_REPO_LOCAL", &metadata.paths.maven_local)
                .env("AGENT_VALIDATION_RUN_ID", &run_id)
                .env("AGENT_VALIDATION_EVENT_ID", event_id.unwrap_or_default())
                .env("AGENT_VALIDATION_EVIDENCE", &evidence_path);
            command.env_remove("GRADLE_RO_DEP_CACHE");
            run_managed_streaming(
                &mut command,
                &build_lock_path,
                &build_lock.token,
                &log_base,
                stdout_to_stderr,
            )
        }
        .context("启动 Gradle Wrapper 失败")?;
        let elapsed = started.elapsed().as_millis();
        let stdout_log = log_base.with_extension("stdout.log");
        let stderr_log = log_base.with_extension("stderr.log");
        let event_id = event_id.map(str::to_string);
        let validation = read_json::<CandidateValidation>(&evidence_path)
            .ok()
            .filter(|value| value.run_id == run_id && Some(&value.event_id) == event_id.as_ref());
        let worktree_clean = clean_before
            && require_clean_worktree(&self.cwd).is_ok()
            && git_output(&self.cwd, &["rev-parse", "HEAD"])
                .is_ok_and(|commit| commit == metadata.commit);
        let receipt = BuildReceipt {
            schema_version: SCHEMA_VERSION,
            runtime_id: metadata.runtime_id.clone(),
            agent_id: metadata.agent_id.clone(),
            worktree_id: metadata.worktree_id.clone(),
            repository: metadata.repository.clone(),
            worktree_path: metadata.worktree_path.clone(),
            commit: metadata.commit.clone(),
            event_id,
            artifact_sha256: validation
                .as_ref()
                .map(|value| value.artifact_sha256.clone()),
            validation,
            worktree_clean,
            command: args.to_vec(),
            started_at,
            finished_at: Utc::now().to_rfc3339(),
            elapsed_ms: elapsed,
            exit_code: status.code(),
            success: status.success(),
            stdout_log: stdout_log.display().to_string(),
            stderr_log: stderr_log.display().to_string(),
        };
        write_json_atomic(
            &PathBuf::from(&metadata.paths.root)
                .join("receipts")
                .join(format!("{run_id}.json")),
            &receipt,
        )?;
        write_json_atomic(
            &PathBuf::from(&metadata.paths.root).join("build-result.json"),
            &receipt,
        )?;
        eprintln!(
            "Gradle 退出码：{}；耗时：{} ms；日志前缀：{}",
            status.code().unwrap_or(-1),
            elapsed,
            log_base.display()
        );
        if !status.success() {
            bail!("Gradle 构建失败，进程退出码 {:?}", status.code());
        }
        Ok(())
    }

    fn gradle_args(
        &self,
        metadata: &Metadata,
        args: &[String],
        workers: Option<&str>,
    ) -> Result<Vec<String>> {
        validate_gradle_isolation_args(args)?;
        if args.iter().any(|arg| {
            arg.starts_with("-Dmaven.repo.local") || arg.starts_with("-Dorg.gradle.jvmargs")
        }) {
            bail!("禁止覆盖运行时管理的 Maven 仓库路径或 Gradle JVM 参数");
        }
        let mut result = vec![format!("-Dmaven.repo.local={}", metadata.paths.maven_local)];
        let test_init =
            PathBuf::from(&metadata.paths.worktree_root).join("runtime-test-isolation.gradle");
        write_text_atomic(
            &test_init,
            include_str!("../scripts/runtime-test-isolation.gradle"),
        )?;
        result.push(format!("-I{}", test_init.display()));
        if !args.iter().any(|argument| argument == "--no-daemon") {
            result.push("--no-daemon".to_string());
        }
        if let Some(count) = workers {
            result.push(format!("--max-workers={count}"));
        }
        if let Ok(heap) = env::var("AGENT_MAX_HEAP") {
            if !valid_heap_size(&heap) {
                bail!("AGENT_MAX_HEAP 格式无效，示例：2g、1536m");
            }
            result.push(format!(
                "-Dorg.gradle.jvmargs=-Xmx{} -Dfile.encoding=UTF-8",
                heap
            ));
        }
        result.extend(args.iter().cloned());
        Ok(result)
    }

    pub fn cleanup(
        &self,
        dry_run: bool,
        purge_cache: bool,
        force: bool,
        worktree_scope: bool,
        worktree_path: Option<&str>,
        worktree_branch: Option<&str>,
    ) -> Result<()> {
        if worktree_scope {
            return self.cleanup_worktree(
                dry_run,
                purge_cache,
                force,
                worktree_path,
                worktree_branch,
            );
        }
        let metadata = self.ensure_metadata(false)?;
        let root = PathBuf::from(&metadata.paths.root);
        self.assert_inside_state_root(&root)?;
        let worktree_root = PathBuf::from(&metadata.paths.worktree_root);
        let worktree_lock = worktree_root.join("locks/worktree.lock");
        let _worktree_guard = StateFileLock::acquire(&worktree_lock)?;
        let gate = worktree_root.join("locks/build-gate.lock");
        recover_state_lock_if_stale(&gate)?;
        let _build_gate = StateFileLock::acquire(&gate)?;
        let runtime_gate = root.join("locks/device-acquire.lock");
        let _runtime_gate = StateFileLock::acquire(&runtime_gate)?;
        let shared_runtime_count = self.count_agent_runtimes(&metadata.worktree_id);
        if purge_cache && shared_runtime_count > 1 {
            bail!("该 Worktree 仍有其他 Agent runtime，不能删除共享 Gradle/Maven 缓存");
        }
        if let Ok(lock) = read_json::<BuildLock>(&worktree_root.join("locks/build.json")) {
            if process_matches(&lock) {
                bail!(
                    "当前运行时的构建锁仍由 PID {} 持有；不能在构建期间清理",
                    lock.pid
                );
            } else if !lock_expired(&lock) {
                bail!(
                    "构建锁由已退出的 PID {} 持有，但租约尚未到期；拒绝清理",
                    lock.pid
                );
            } else if lock.owner == metadata.agent_id {
                remove_lock_if_unchanged(&worktree_root.join("locks/build.json"), &lock)?;
                let _ = fs::remove_file(root.join("locks/build.token"));
            }
        }
        let build_lock_path = worktree_root.join("locks/build.json");
        if build_lock_path.exists() && read_json::<BuildLock>(&build_lock_path).is_err() {
            bail!("构建锁内容无效；拒绝清理未知锁文件");
        }
        let operation_path = root.join("locks/device-operation.json");
        if operation_path.exists() && read_json::<TestLease>(&operation_path).is_err() {
            bail!("设备操作锁内容无效；拒绝清理未知锁文件");
        }
        if let Ok(operation) = read_json::<TestLease>(&operation_path) {
            if process_identity_alive(operation.pid, &operation.process_start_time) {
                bail!(
                    "设备 {} 正由 PID {} 操作，不能清理",
                    operation.serial,
                    operation.pid
                );
            }
        }
        if dry_run {
            println!(
                "将清理运行时状态：{}{}",
                metadata.runtime_id,
                if purge_cache {
                    "，并删除本 Worktree 的 Gradle/Maven 缓存"
                } else {
                    "；保留 Gradle/Maven 缓存"
                }
            );
            return Ok(());
        }
        if purge_cache {
            self.assert_inside_state_root(&worktree_root)?;
            stop_gradle_daemons(
                Path::new(&metadata.worktree_path),
                &metadata.paths.gradle_user_home,
            )
            .context("停止 Gradle Daemon 失败；未释放租约或删除缓存")?;
        }
        // 共享 Worktree 构建锁由其令牌所有者释放；Agent 清理不得删除其他 Agent 的锁。
        if let Ok(lease) = read_json::<DeviceLease>(&root.join("device-lease.json")) {
            remove_device_lease(&self.state_root, &lease)?;
            let _ = fs::remove_file(root.join("device-lease.json"));
            let _ = fs::remove_file(root.join("device-lease.token"));
        }
        let _ = fs::remove_file(root.join("locks/device-operation.json"));
        if purge_cache {
            for cache in [
                worktree_root.join("gradle-user-home"),
                worktree_root.join("maven-local"),
                worktree_root.join("test-jvm"),
            ] {
                self.assert_inside_state_root(&cache)?;
                if cache.exists() {
                    fs::remove_dir_all(&cache)
                        .with_context(|| format!("删除缓存失败：{}", cache.display()))?;
                }
            }
        }
        release_ports(&self.state_root, &metadata.runtime_id, &metadata.agent_id)?;
        record_event(
            &self.state_root,
            "runtime_cleaned",
            serde_json::json!({"runtime_id": metadata.runtime_id, "purge_cache": purge_cache}),
        )?;
        let mut updated = metadata;
        updated.status = "cleaned".to_string();
        updated.last_seen_at = Utc::now().to_rfc3339();
        write_json_atomic(&root.join("metadata.json"), &updated)?;
        println!("运行时已清理：{}", updated.runtime_id);
        Ok(())
    }

    fn cleanup_worktree(
        &self,
        dry_run: bool,
        purge_cache: bool,
        force: bool,
        worktree_path: Option<&str>,
        worktree_branch: Option<&str>,
    ) -> Result<()> {
        let (id, worktree_cwd) = if let Some(branch) = worktree_branch {
            self.find_worktree_by_branch(branch)?
        } else if let Some(path) = worktree_path {
            self.find_worktree_by_path(path)?
        } else {
            let git = self.git_info()?;
            if git.is_primary {
                bail!("拒绝对主 Worktree 执行 Worktree 级清理");
            }
            (worktree_id(&git.git_dir), git.worktree)
        };
        let worktree_root = self.state_root.join("worktrees").join(&id);
        self.assert_inside_state_root(&worktree_root)?;
        fs::create_dir_all(&worktree_root)?;
        let record_path = worktree_root.join("worktree.json");
        if read_json::<serde_json::Value>(&record_path)
            .ok()
            .and_then(|record| {
                record
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .as_deref()
            == Some("removed")
        {
            println!("Worktree runtime 已回收：{}", id);
            return Ok(());
        }
        let _worktree_record_guard =
            StateFileLock::acquire(&worktree_root.join("locks/worktree.lock"))?;
        let gate = worktree_root.join("locks/build-gate.lock");
        recover_state_lock_if_stale(&gate)?;
        let _build_gate = StateFileLock::acquire(&gate)?;
        let mut runtimes = Vec::new();
        let mut gates = Vec::new();
        if let Ok(agent_dirs) = self.state_root.join("agents").read_dir() {
            for agent in agent_dirs.filter_map(Result::ok) {
                let runtime = agent.path().join(&id);
                if read_json::<Metadata>(&runtime.join("metadata.json")).is_ok() {
                    runtimes.push(runtime);
                }
            }
        }
        runtimes.sort();
        let mut agents = Vec::new();
        if let Ok(agent_dirs) = self.state_root.join("agents").read_dir() {
            for agent in agent_dirs.filter_map(Result::ok) {
                let runtime = agent.path().join(&id);
                let path = runtime.join("metadata.json");
                if let Ok(metadata) = read_json::<Metadata>(&path) {
                    agents.push((runtime, metadata));
                }
            }
        }
        for runtime in &runtimes {
            gates.push(StateFileLock::acquire(
                &runtime.join("locks/device-acquire.lock"),
            )?);
        }
        let active_runtimes = agents
            .iter()
            .filter(|(_, metadata)| metadata.status == "active")
            .collect::<Vec<_>>();
        if active_runtimes.len() > 1 {
            let owners = active_runtimes
                .iter()
                .map(|(_, metadata)| metadata.agent_id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Worktree 仍有多个 active Agent runtime ({owners})；先分别执行 Agent cleanup，再移除 Worktree"
            );
        }
        let build_path = worktree_root.join("locks/build.json");
        if let Ok(lock) = read_json::<BuildLock>(&build_path) {
            if process_matches(&lock) {
                bail!(
                    "Worktree 仍有 Agent {} 的构建 (PID {}) 在运行，不能移除",
                    lock.owner,
                    lock.pid
                );
            }
            if !lock_expired(&lock) {
                bail!(
                    "Worktree 构建锁由已退出的 PID {} 持有但尚未到期，拒绝清理",
                    lock.pid
                );
            }
        }
        for (runtime, metadata) in &agents {
            let operation_path = runtime.join("locks/device-operation.json");
            if operation_path.exists() && read_json::<TestLease>(&operation_path).is_err() {
                bail!(
                    "Agent {} 的设备操作锁内容无效；拒绝 Worktree 清理",
                    metadata.agent_id
                );
            }
            if let Ok(operation) = read_json::<TestLease>(&operation_path) {
                if process_identity_alive(operation.pid, &operation.process_start_time) {
                    bail!(
                        "Worktree 仍有 Agent {} 使用设备 {}，不能移除",
                        metadata.agent_id,
                        operation.serial
                    );
                }
            }
            if let Ok(lease) = read_json::<DeviceLease>(&runtime.join("device-lease.json")) {
                if !lock_expired_fields(&lease.expires_at) && !force {
                    bail!(
                        "Agent {} 仍租用设备 {}；使用 --force 只回收租约，不会中断设备操作",
                        metadata.agent_id,
                        lease.serial
                    );
                }
            }
        }
        let build_lock_path = worktree_root.join("locks/build.json");
        if build_lock_path.exists() && read_json::<BuildLock>(&build_lock_path).is_err() {
            bail!("Worktree 构建锁内容无效；拒绝清理未知锁文件");
        }
        if dry_run {
            println!(
                "将清理 Worktree {} 的 {} 个 Agent runtime{}",
                id,
                agents.len(),
                if purge_cache {
                    "并删除共享缓存"
                } else {
                    "并保留共享缓存"
                }
            );
            return Ok(());
        }
        let home = worktree_root.join("gradle-user-home");
        if purge_cache {
            stop_gradle_daemons(&worktree_cwd, &home.display().to_string())
                .context("停止 Worktree Gradle Daemon 失败；未回收运行时")?;
        }
        let build_lock = worktree_root.join("locks/build.json");
        if build_lock.exists() {
            fs::remove_file(&build_lock).context("移除过期 Worktree 构建锁失败")?;
        }
        let _ = fs::remove_file(worktree_root.join("locks/build.token"));
        for (runtime, metadata) in &agents {
            if let Ok(lease) = read_json::<DeviceLease>(&runtime.join("device-lease.json")) {
                remove_device_lease(&self.state_root, &lease)?;
            }
            let _ = fs::remove_file(runtime.join("device-lease.json"));
            let _ = fs::remove_file(runtime.join("device-lease.token"));
            let _ = fs::remove_file(runtime.join("locks/device-operation.json"));
            let _ = fs::remove_file(runtime.join("locks/build.token"));
            release_ports(&self.state_root, &metadata.runtime_id, &metadata.agent_id)?;
            let mut updated = metadata.clone();
            updated.status = "cleaned".to_string();
            updated.last_seen_at = Utc::now().to_rfc3339();
            write_json_atomic(&runtime.join("metadata.json"), &updated)?;
        }
        if purge_cache {
            for path in [
                home,
                worktree_root.join("maven-local"),
                worktree_root.join("test-jvm"),
            ] {
                self.assert_inside_state_root(&path)?;
                if path.exists() {
                    fs::remove_dir_all(&path)
                        .with_context(|| format!("删除共享缓存失败：{}", path.display()))?;
                }
            }
        }
        let mut record = read_json::<serde_json::Value>(&record_path)
            .unwrap_or_else(|_| serde_json::json!({"runtime_id": id}));
        record["status"] = serde_json::json!("removed");
        record["last_seen_at"] = serde_json::json!(Utc::now().to_rfc3339());
        write_json_atomic(&record_path, &record)?;
        record_event(
            &self.state_root,
            "worktree_cleaned",
            serde_json::json!({"runtime_id": id, "agents": agents.len(), "purge_cache": purge_cache}),
        )?;
        println!("Worktree runtime 已回收：{}", id);
        Ok(())
    }

    fn find_worktree_by_path(&self, requested_path: &str) -> Result<(String, PathBuf)> {
        let requested = path_identity(Path::new(requested_path));
        if !Path::new(requested_path).is_absolute() {
            bail!("--worktree-path 必须是绝对路径");
        }
        let agents_root = self.state_root.join("agents");
        let mut matches = Vec::new();
        if let Ok(agent_dirs) = agents_root.read_dir() {
            for agent in agent_dirs.filter_map(Result::ok) {
                let Ok(runtimes) = agent.path().read_dir() else {
                    continue;
                };
                for runtime in runtimes.filter_map(Result::ok) {
                    let metadata_path = runtime.path().join("metadata.json");
                    let Ok(metadata) = read_json::<Metadata>(&metadata_path) else {
                        continue;
                    };
                    if !metadata.is_primary
                        && path_identity(Path::new(&metadata.worktree_path)) == requested
                    {
                        matches.push((metadata.worktree_id, PathBuf::from(metadata.worktree_path)));
                    }
                }
            }
        }
        matches.sort_by(|left, right| left.0.cmp(&right.0));
        matches.dedup_by(|left, right| left.0 == right.0);
        match matches.len() {
            0 => bail!("没有找到路径匹配的非主 Worktree 运行时：{requested_path}"),
            1 => Ok(matches.remove(0)),
            _ => bail!("Worktree 路径匹配到多个运行时，拒绝执行清理"),
        }
    }

    fn find_worktree_by_branch(&self, requested_branch: &str) -> Result<(String, PathBuf)> {
        if requested_branch.is_empty() {
            bail!("--worktree-branch 不能为空");
        }
        let current_repository = self.git_info()?.repository;
        let current_repository = path_identity(&current_repository);
        let mut matches = Vec::new();
        if let Ok(agent_dirs) = self.state_root.join("agents").read_dir() {
            for agent in agent_dirs.filter_map(Result::ok) {
                let Ok(runtimes) = agent.path().read_dir() else {
                    continue;
                };
                for runtime in runtimes.filter_map(Result::ok) {
                    let metadata_path = runtime.path().join("metadata.json");
                    let Ok(metadata) = read_json::<Metadata>(&metadata_path) else {
                        continue;
                    };
                    let record_path = self
                        .state_root
                        .join("worktrees")
                        .join(&metadata.worktree_id)
                        .join("worktree.json");
                    let record_branch =
                        read_json::<serde_json::Value>(&record_path)
                            .ok()
                            .and_then(|record| {
                                record
                                    .get("branch")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_string)
                            });
                    let branch_matches = metadata.branch.as_deref() == Some(requested_branch)
                        || record_branch.as_deref() == Some(requested_branch);
                    if !metadata.is_primary
                        && branch_matches
                        && path_identity(Path::new(&metadata.repository)) == current_repository
                        && Path::new(&metadata.repository).exists()
                    {
                        matches.push((
                            metadata.worktree_id,
                            PathBuf::from(metadata.worktree_path),
                            PathBuf::from(metadata.repository),
                        ));
                    }
                }
            }
        }
        matches.sort_by(|left, right| left.0.cmp(&right.0));
        matches.dedup_by(|left, right| left.0 == right.0);
        match matches.len() {
            0 => bail!("没有找到分支匹配的非主 Worktree 运行时：{requested_branch}"),
            1 => {
                let (id, path, repository) = matches.remove(0);
                Ok((id, if path.exists() { path } else { repository }))
            }
            _ => bail!("分支名匹配到多个仓库 Worktree，拒绝执行清理"),
        }
    }

    pub fn doctor(&self, json: bool) -> Result<()> {
        let git = self.git_info();
        let metadata = self
            .find_metadata_path()
            .ok()
            .and_then(|path| read_json::<Metadata>(&path).ok());
        let mut checks = vec![
            status_check("Git", command_exists("git")),
            status_check("Worktrunk", worktrunk_available()),
            status_check("当前目录是合法 Git Worktree", git.is_ok()),
            status_check(
                "Gradle Wrapper",
                self.cwd
                    .join(if cfg!(windows) {
                        "gradlew.bat"
                    } else {
                        "gradlew"
                    })
                    .is_file(),
            ),
            status_check("JDK", command_exists("java")),
            status_check("Android SDK", android_sdk_path().is_some()),
            status_check("ADB", command_exists("adb")),
            status_check(
                "PowerShell",
                !cfg!(windows) || command_exists("powershell.exe"),
            ),
            status_check(
                "Windows Terminal wt.exe 命令冲突",
                !cfg!(windows) || !windows_terminal_wt_conflict(),
            ),
        ];
        if let Some(metadata) = metadata {
            if let Err(error) = self.recover_stale_resources(&metadata) {
                checks.push(serde_json::json!({"name":"回收过期资源","status":"WARN","error":format!("{error:#}")}));
            } else {
                checks.push(status_check("回收过期资源", true));
            }
            checks.push(status_check(
                "运行时目录存在",
                Path::new(&metadata.paths.root).is_dir(),
            ));
            checks.push(status_check(
                "独立 Gradle 用户目录",
                Path::new(&metadata.paths.gradle_user_home).starts_with(&self.state_root),
            ));
            checks.push(status_check(
                "独立 Maven 本地目录",
                Path::new(&metadata.paths.maven_local).starts_with(&self.state_root),
            ));
            let lock_path = PathBuf::from(&metadata.paths.worktree_root).join("locks/build.json");
            match read_build_lock_consistently(&lock_path) {
                Ok(lock) => checks.push(serde_json::json!({"name":"构建锁租约","status":if lock_expired(&lock) && !process_matches(&lock) {"WARN"} else {"PASS"},"owner":lock.owner,"pid":lock.pid,"gradle_pid":lock.gradle_pid,"expires_at":lock.expires_at})),
                Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
                Err(error) => checks.push(serde_json::json!({"name":"构建锁状态","status":"WARN","error":format!("{error:#}")})),
            }
            if let Ok(lease) = read_json::<DeviceLease>(
                &PathBuf::from(&metadata.paths.root).join("device-lease.json"),
            ) {
                let stale = lock_expired_fields(&lease.expires_at);
                checks.push(serde_json::json!({"name":"设备租约","status":if stale {"WARN"} else {"PASS"},"serial":lease.serial,"owner":lease.agent_id,"expires_at":lease.expires_at}));
            }
            if let Ok(test_lease) = read_json::<TestLease>(
                &PathBuf::from(&metadata.paths.root).join("locks/device-operation.json"),
            ) {
                let active = !lock_expired_fields(&test_lease.expires_at)
                    && process_identity_alive(test_lease.pid, &test_lease.process_start_time);
                checks.push(serde_json::json!({"name":"设备操作锁","status":if active {"PASS"} else {"WARN"},"serial":test_lease.serial,"owner":test_lease.agent_id,"pid":test_lease.pid,"expires_at":test_lease.expires_at}));
            }
        } else {
            checks.push(status_check("当前 Worktree 已准备", false));
        }
        match probe::adb_devices() {
            Ok(output) => checks.push(serde_json::json!({
                "name":"ADB 设备查询",
                "status":if output.status.success() {"PASS"} else {"FAIL"},
                "exit_code":output.status.code(),
                "devices":parse_adb_device_statuses(&String::from_utf8_lossy(&output.stdout))
            })),
            Err(error) => checks.push(serde_json::json!({"name":"ADB 设备查询","status":"FAIL","error":error.to_string()})),
        }
        let report = serde_json::json!({"checks":checks});
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            for check in report["checks"].as_array().expect("checks array") {
                println!("{} {}", check["status"], check["name"]);
            }
        }
        let has_failure = report["checks"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["status"] == "FAIL"));
        if has_failure {
            bail!("doctor 检查发现 FAIL 项");
        }
        Ok(())
    }

    fn recover_stale_resources(&self, metadata: &Metadata) -> Result<()> {
        let root = PathBuf::from(&metadata.paths.root);
        {
            let gate_path =
                PathBuf::from(&metadata.paths.worktree_root).join("locks/build-gate.lock");
            recover_state_lock_if_stale(&gate_path)?;
            let _gate = StateFileLock::acquire(&gate_path)?;
            self.recover_stale_worktree_lock(metadata)?;
        }
        {
            let gate_path =
                PathBuf::from(&metadata.paths.worktree_root).join("locks/build-gate.lock");
            let _gate = StateFileLock::acquire(&gate_path)?;
            let runtime_gate = root.join("locks/device-acquire.lock");
            let _runtime_gate = StateFileLock::acquire(&runtime_gate)?;
            let local_lease_path = root.join("device-lease.json");
            if let Ok(lease) = read_json::<DeviceLease>(&local_lease_path) {
                if lock_expired_fields(&lease.expires_at) {
                    remove_device_lease(&self.state_root, &lease)?;
                    let _ = fs::remove_file(&local_lease_path);
                    let _ = fs::remove_file(root.join("device-lease.token"));
                    record_event(
                        &self.state_root,
                        "device_lease_recovered",
                        serde_json::json!({"runtime_id": lease.runtime_id, "serial": lease.serial, "owner": lease.agent_id}),
                    )?;
                }
            }
            let operation_path = root.join("locks/device-operation.json");
            if let Ok(operation) = read_json::<TestLease>(&operation_path) {
                if !process_identity_alive(operation.pid, &operation.process_start_time) {
                    let snapshot = fs::read_to_string(&operation_path)?;
                    if read_json::<TestLease>(&operation_path)
                        .is_ok_and(|latest| latest.token == operation.token)
                        && fs::read_to_string(&operation_path)
                            .is_ok_and(|latest| latest == snapshot)
                    {
                        fs::remove_file(&operation_path)?;
                        record_event(
                            &self.state_root,
                            "device_operation_recovered",
                            serde_json::json!({"runtime_id": operation.runtime_id, "serial": operation.serial, "pid": operation.pid}),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn recover_stale_worktree_lock(&self, metadata: &Metadata) -> Result<()> {
        let worktree_root = PathBuf::from(&metadata.paths.worktree_root);
        let path = worktree_root.join("locks/build.json");
        let lock = match read_json::<BuildLock>(&path) {
            Ok(lock) => lock,
            Err(_) => return Ok(()),
        };
        let creator_alive = process_identity_alive(lock.pid, &lock.process_start_time);
        let gradle_alive = lock
            .gradle_pid
            .zip(lock.gradle_process_start_time.as_deref())
            .is_some_and(|(pid, start)| process_identity_alive(pid, start));
        if creator_alive || gradle_alive {
            if lock_expired(&lock) {
                bail!("Worktree 构建锁租约已过期，但构建进程仍运行；拒绝抢占");
            }
            return Ok(());
        }
        if lock.gradle_pid.is_none() && !lock_expired(&lock) {
            bail!(
                "构建锁由已退出的 PID {} 持有，但租约尚未到期；拒绝提前回收",
                lock.pid
            );
        }
        let snapshot = fs::read_to_string(&path)?;
        if read_json::<BuildLock>(&path).is_ok_and(|current| current.token == lock.token)
            && fs::read_to_string(&path).is_ok_and(|current| current == snapshot)
        {
            fs::remove_file(&path)?;
            let _ = fs::remove_file(worktree_root.join("locks/build.token"));
            record_event(
                &self.state_root,
                "build_lock_recovered",
                serde_json::json!({"runtime_id": lock.runtime_id, "owner": lock.owner, "pid": lock.pid}),
            )?;
        }
        Ok(())
    }

    fn ensure_metadata(&self, allow_primary: bool) -> Result<Metadata> {
        let path = self.find_metadata_path()?;
        match read_json::<Metadata>(&path) {
            Ok(mut metadata) => {
                let worktree_lock =
                    PathBuf::from(&metadata.paths.worktree_root).join("locks/worktree.lock");
                let _worktree_guard = StateFileLock::acquire(&worktree_lock)?;
                let worktree_record_path =
                    PathBuf::from(&metadata.paths.worktree_root).join("worktree.json");
                if read_json::<serde_json::Value>(&worktree_record_path)
                    .ok()
                    .is_some_and(|record| {
                        record.get("status").and_then(serde_json::Value::as_str) == Some("removed")
                    })
                {
                    bail!("Worktree runtime 已标记删除，拒绝重新激活");
                }
                let git = self.git_info()?;
                let expected_worktree_id = worktree_id(&git.git_dir);
                if metadata.worktree_id != expected_worktree_id {
                    bail!("运行时 Worktree 身份与当前 Git Worktree 不符；拒绝复用缓存");
                }
                if metadata.worktree_path != git.worktree.display().to_string() {
                    bail!(
                        "运行时元数据中的 Worktree 路径与当前 Git Worktree 不符；拒绝复用错误缓存"
                    );
                }
                metadata.branch = git.branch;
                metadata.commit = git.commit;
                metadata.last_seen_at = Utc::now().to_rfc3339();
                metadata.status = "active".to_string();
                write_json_atomic(&path, &metadata)?;
                let worktree_record_path =
                    PathBuf::from(&metadata.paths.worktree_root).join("worktree.json");
                if let Ok(mut record) = read_json::<serde_json::Value>(&worktree_record_path) {
                    record["branch"] = serde_json::json!(metadata.branch);
                    record["commit"] = serde_json::json!(metadata.commit);
                    record["last_seen_at"] = serde_json::json!(metadata.last_seen_at);
                    record["status"] = serde_json::json!("active");
                    write_json_atomic(&worktree_record_path, &record)?;
                }
                Ok(metadata)
            }
            Err(_) => self.prepare(PrepareOptions { allow_primary }),
        }
    }

    fn count_agent_runtimes(&self, runtime_id: &str) -> usize {
        self.state_root
            .join("agents")
            .read_dir()
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|agent| {
                agent
                    .path()
                    .join(runtime_id)
                    .join("metadata.json")
                    .exists()
                    .then_some(agent.path().join(runtime_id).join("metadata.json"))
            })
            .filter_map(|path| read_json::<Metadata>(&path).ok())
            .filter(|metadata| metadata.status == "active")
            .count()
    }

    fn find_metadata_path(&self) -> Result<PathBuf> {
        let git = self.git_info()?;
        let runtime_id = worktree_id(&git.git_dir);
        let agent_id = self
            .agent_id
            .clone()
            .unwrap_or_else(|| format!("codex-{}", runtime_id));
        Ok(self
            .state_root
            .join("agents")
            .join(safe_component(&agent_id))
            .join(runtime_id)
            .join("metadata.json"))
    }

    fn git_info(&self) -> Result<GitInfo> {
        let worktree = PathBuf::from(git_output(&self.cwd, &["rev-parse", "--show-toplevel"])?);
        let worktree = fs::canonicalize(worktree).context("规范化 Worktree 路径失败")?;
        let git_dir = PathBuf::from(git_output(&self.cwd, &["rev-parse", "--absolute-git-dir"])?);
        let common_dir = PathBuf::from(git_output(
            &self.cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?);
        let list = git_output(&self.cwd, &["worktree", "list", "--porcelain"])?;
        let primary = list
            .lines()
            .find_map(|line| line.strip_prefix("worktree "))
            .map(PathBuf::from)
            .and_then(|path| fs::canonicalize(path).ok());
        // 非标准 Git 管理目录不能用父目录推断仓库身份，避免不同仓库共享同一归属。
        if primary.as_ref().is_none_or(|root| {
            !root.join(".git").is_dir()
                || path_identity(&root.join(".git")) != path_identity(&common_dir)
        }) {
            bail!(
                "暂不支持裸仓库、子模块或外置 Git 管理目录；无法安全确定仓库身份，请使用标准独立克隆和 Worktree"
            );
        }
        let branch = Command::new("git")
            .current_dir(&self.cwd)
            .args(["symbolic-ref", "--short", "-q", "HEAD"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|name| !name.is_empty());
        Ok(GitInfo {
            repository: common_dir.parent().unwrap_or(&common_dir).to_path_buf(),
            is_primary: primary.as_deref() == Some(worktree.as_path()),
            worktree,
            git_dir,
            branch,
            commit: git_output(&self.cwd, &["rev-parse", "HEAD"])?,
        })
    }

    fn assert_inside_state_root(&self, path: &Path) -> Result<()> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        {
            bail!("清理目标必须是无上级跳转的绝对路径：{}", path.display());
        }
        let state_root =
            fs::canonicalize(&self.state_root).context("规范化运行时状态根目录失败")?;
        let mut ancestor = path;
        let mut missing = Vec::new();
        while !ancestor.exists() {
            missing.push(
                ancestor
                    .file_name()
                    .ok_or_else(|| anyhow::anyhow!("清理目标路径无效：{}", path.display()))?,
            );
            ancestor = ancestor
                .parent()
                .ok_or_else(|| anyhow::anyhow!("清理目标路径无父目录：{}", path.display()))?;
        }
        let mut target = fs::canonicalize(ancestor).context("规范化清理目标父目录失败")?;
        for part in missing.into_iter().rev() {
            target.push(part);
        }
        let root_identity = path_identity(&state_root);
        let target_identity = path_identity(&target);
        if target_identity != root_identity
            && !target_identity.starts_with(&format!("{root_identity}\\"))
        {
            bail!("目标路径越出运行时状态根目录：{}", target.display());
        }
        Ok(())
    }
}

#[derive(Debug)]
struct GitInfo {
    repository: PathBuf,
    worktree: PathBuf,
    git_dir: PathBuf,
    branch: Option<String>,
    commit: String,
    is_primary: bool,
}

fn path_identity(path: &Path) -> String {
    let identity = fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_string();
    let identity = identity.strip_prefix("\\\\?\\").unwrap_or(&identity);
    identity.replace('/', "\\").to_ascii_lowercase()
}

fn acquire_lock_internal(
    runtime: &Runtime,
    timeout: u64,
    lease: u64,
    owner: Option<&str>,
) -> Result<String> {
    let metadata = runtime.ensure_metadata(false)?;
    let lock_path = PathBuf::from(&metadata.paths.worktree_root).join("locks/build.json");
    let gate_path = PathBuf::from(&metadata.paths.worktree_root).join("locks/build-gate.lock");
    let started = Instant::now();
    loop {
        let _gate = StateFileLock::acquire(&gate_path)?;
        let now = Utc::now();
        let token = unique_token();
        let lock = BuildLock {
            schema_version: SCHEMA_VERSION,
            runtime_id: metadata.runtime_id.clone(),
            owner: owner.unwrap_or(&metadata.agent_id).to_string(),
            pid: std::process::id(),
            process_start_time: process_start_time(),
            token: token.clone(),
            created_at: now.to_rfc3339(),
            expires_at: (now + Duration::seconds(lease.min(i64::MAX as u64) as i64)).to_rfc3339(),
            heartbeat_at: now.to_rfc3339(),
            gradle_pid: None,
            gradle_process_start_time: None,
        };
        let mut candidate = NamedTempFile::new_in(lock_path.parent().expect("lock parent"))?;
        candidate.write_all(serde_json::to_string_pretty(&lock)?.as_bytes())?;
        candidate.as_file().sync_all()?;
        match candidate.persist_noclobber(&lock_path) {
            Ok(_) => return Ok(token),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                match read_json::<BuildLock>(&lock_path) {
                    Ok(existing) if lock_expired(&existing) && !process_matches(&existing) => {
                        remove_lock_if_unchanged(&lock_path, &existing)?;
                    }
                    Ok(existing) if timeout == 0 => bail!(
                        "构建锁由 {} (PID {}) 持有，租约到期 {}",
                        existing.owner,
                        existing.pid,
                        existing.expires_at
                    ),
                    Ok(_) => {
                        if started.elapsed().as_secs() >= timeout {
                            bail!("等待构建锁超时");
                        }
                        thread::sleep(StdDuration::from_millis(200));
                    }
                    Err(_) => bail!("锁文件存在但无效，拒绝覆盖：{}", lock_path.display()),
                }
            }
            Err(error) => return Err(error.error).context("原子获取构建锁失败"),
        }
    }
}

fn remove_lock_if_unchanged(path: &Path, expected: &BuildLock) -> Result<()> {
    let current = read_json::<BuildLock>(path).context("重新读取构建锁失败")?;
    if current.token != expected.token {
        bail!("构建锁已被其他持有者更新，拒绝删除");
    }
    fs::remove_file(path).with_context(|| format!("删除当前构建锁失败：{}", path.display()))
}

fn lock_expired(lock: &BuildLock) -> bool {
    lock_expired_fields(&lock.expires_at)
}

fn lock_expired_fields(expires_at: &str) -> bool {
    DateTime::parse_from_rfc3339(expires_at)
        .map(|expiry| expiry.with_timezone(&Utc) <= Utc::now())
        .unwrap_or(true)
}

fn process_start_time() -> String {
    process_start_time_for_pid(std::process::id())
}

fn process_start_time_for_pid(pid: u32) -> String {
    #[cfg(windows)]
    {
        let output = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &format!("(Get-Process -Id {} -ErrorAction Stop).StartTime.ToUniversalTime().ToString('o')", pid)]).output();
        if let Ok(output) = output {
            if output.status.success() {
                return String::from_utf8_lossy(&output.stdout).trim().to_string();
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
            if let Some(start) = stat.split_whitespace().nth(21) {
                return start.to_string();
            }
        }
    }
    Utc::now().to_rfc3339()
}

#[cfg(test)]
fn live_process_identity(pid: u32) -> String {
    process_start_time_for_pid(pid)
}

fn process_matches(lock: &BuildLock) -> bool {
    match (lock.gradle_pid, lock.gradle_process_start_time.as_deref()) {
        (Some(pid), Some(start_time)) if process_identity_alive(pid, start_time) => return true,
        (Some(_), None) => return true,
        _ => {}
    }
    #[cfg(windows)]
    {
        let expected = base64(lock.process_start_time.as_bytes());
        let script = format!(
            "$p=Get-Process -Id {} -ErrorAction SilentlyContinue; if ($null -eq $p) {{ exit 1 }}; $t=$p.StartTime.ToUniversalTime().ToString('o'); $e=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(\"{}\")); if ($t -eq $e) {{ exit 0 }} else {{ exit 2 }}",
            lock.pid, expected
        );
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        let path = PathBuf::from(format!("/proc/{}/stat", lock.pid));
        fs::read_to_string(path)
            .ok()
            .and_then(|stat| stat.split_whitespace().nth(21).map(str::to_string))
            .is_some_and(|start| start == lock.process_start_time)
    }
}

fn process_identity_alive(pid: u32, start_time: &str) -> bool {
    #[cfg(windows)]
    {
        let expected = base64(start_time.as_bytes());
        let script = format!(
            "$p=Get-Process -Id {} -ErrorAction SilentlyContinue; if ($null -eq $p) {{ exit 1 }}; $t=$p.StartTime.ToUniversalTime().ToString('o'); $e=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(\"{}\")); if ($t -eq $e) {{ exit 0 }} else {{ exit 2 }}",
            pid, expected
        );
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.split_whitespace().nth(21).map(str::to_string))
            .is_some_and(|start| start == start_time)
    }
}

fn available_devices() -> Result<Vec<String>> {
    let output = probe::adb_devices()?;
    if !output.status.success() {
        bail!("adb devices 失败，退出码 {:?}", output.status.code());
    }
    let text = String::from_utf8(output.stdout)?;
    Ok(parse_adb_devices(&text))
}

#[cfg(test)]
fn adb_devices_with(program: &Path) -> Result<Vec<String>> {
    let output = Command::new(program).arg("devices").output()?;
    if !output.status.success() {
        bail!("模拟 ADB 查询失败");
    }
    Ok(parse_adb_devices(&String::from_utf8(output.stdout)?))
}

fn parse_adb_devices(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .filter_map(parse_adb_device_line)
        .filter(|(_, status)| *status == "device")
        .map(|(serial, _)| serial.to_string())
        .collect()
}

fn parse_adb_device_line(line: &str) -> Option<(&str, &str)> {
    // ADB 用制表符分隔设备与状态；无线设备名称中的空格属于序列号。
    let (serial, details) = if let Some((serial, details)) = line.split_once('\t') {
        (serial.trim(), details.trim())
    } else {
        let offset = line.find(char::is_whitespace)?;
        (line[..offset].trim(), line[offset..].trim())
    };
    if serial.is_empty() || details.is_empty() {
        return None;
    }
    let status = if details.starts_with("no permissions") {
        "no permissions"
    } else {
        details.split_ascii_whitespace().next()?
    };
    Some((serial, status))
}

fn validate_adb_args(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("缺少 ADB 子命令");
    }
    let first = &args[0];
    if first.starts_with('-')
        || matches!(first.as_str(), "start-server" | "kill-server")
        || ["-s", "-d", "-e", "-t", "-H", "-P", "--one-device"]
            .iter()
            .any(|option| first == option || first.starts_with(&format!("{option}=")))
    {
        bail!("禁止覆盖租约设备或操作共享 ADB Server；设备序列号由租约注入");
    }
    Ok(())
}

fn parse_adb_device_statuses(text: &str) -> Vec<serde_json::Value> {
    text.lines()
        .skip(1)
        .filter_map(parse_adb_device_line)
        .map(|(serial, status)| serde_json::json!({"serial": serial, "status": status}))
        .collect()
}

#[cfg(test)]
fn acquire_device_lease_from_devices(
    runtime: &Runtime,
    metadata: &Metadata,
    serial: &str,
    lease_seconds: u64,
    devices: &[String],
) -> Result<DeviceLease> {
    if !devices.iter().any(|candidate| candidate == serial) {
        bail!("设备 {} 未连接或状态不是 device", serial);
    }
    let worktree_gate = PathBuf::from(&metadata.paths.worktree_root).join("locks/build-gate.lock");
    let _worktree_guard = StateFileLock::acquire(&worktree_gate)?;
    let runtime_gate = PathBuf::from(&metadata.paths.root).join("locks/device-acquire.lock");
    let _runtime_guard = StateFileLock::acquire(&runtime_gate)?;
    acquire_device_lease_from_devices_locked(runtime, metadata, serial, lease_seconds, devices)
}

fn acquire_device_lease_from_devices_locked(
    runtime: &Runtime,
    metadata: &Metadata,
    serial: &str,
    lease_seconds: u64,
    devices: &[String],
) -> Result<DeviceLease> {
    if !devices.iter().any(|candidate| candidate == serial) {
        bail!("设备 {} 未连接或状态不是 device", serial);
    }
    let global_path = device_lease_path(&runtime.state_root, serial);
    let local_path = PathBuf::from(&metadata.paths.root).join("device-lease.json");
    match read_json::<DeviceLease>(&local_path) {
        Ok(existing)
            if existing.runtime_id == metadata.runtime_id
                && existing.agent_id == metadata.agent_id
                && !lock_expired_fields(&existing.expires_at) =>
        {
            bail!(
                "当前 Agent 已租用设备 {}；释放现有租约后再申请其他设备",
                existing.serial
            );
        }
        Ok(existing)
            if existing.runtime_id != metadata.runtime_id
                || existing.agent_id != metadata.agent_id =>
        {
            bail!("本地设备租约身份与当前 Agent 不匹配；拒绝覆盖");
        }
        Err(_) if local_path.exists() => {
            bail!(
                "本地设备租约文件无法解析：{}；拒绝覆盖",
                local_path.display()
            );
        }
        _ => {}
    }
    fs::create_dir_all(global_path.parent().expect("device registry parent"))?;
    let _guard = StateFileLock::acquire(&global_path.with_extension("lock"))?;
    if let Ok(existing) = read_json::<DeviceLease>(&global_path) {
        if !lock_expired_fields(&existing.expires_at) {
            bail!(
                "设备 {} 已由 Agent {} (PID {}) 租用，到期 {}",
                serial,
                existing.agent_id,
                existing.pid,
                existing.expires_at
            );
        }
    } else if global_path.exists() {
        bail!(
            "设备租约文件已存在但无法解析：{}；拒绝覆盖",
            global_path.display()
        );
    }
    let now = Utc::now();
    let lease = DeviceLease {
        schema_version: SCHEMA_VERSION,
        runtime_id: metadata.runtime_id.clone(),
        agent_id: metadata.agent_id.clone(),
        serial: serial.to_string(),
        pid: std::process::id(),
        process_start_time: process_start_time(),
        token: unique_token(),
        acquired_at: now.to_rfc3339(),
        expires_at: (now + Duration::seconds(lease_seconds.min(i64::MAX as u64) as i64))
            .to_rfc3339(),
        heartbeat_at: now.to_rfc3339(),
        lease_seconds,
        status: "active".to_string(),
    };
    write_json_atomic(&global_path, &lease)?;
    if let Err(error) = write_json_atomic(&local_path, &lease) {
        let current = read_json::<DeviceLease>(&global_path)
            .context("写入本地租约失败，且无法确认全局租约归属")?;
        if current.token == lease.token && current.runtime_id == lease.runtime_id {
            fs::remove_file(&global_path).context("本地租约写入失败，回滚全局租约也失败")?;
        }
        return Err(error).context("写入本地设备租约失败；已回滚本次全局租约");
    }
    let token_path = PathBuf::from(&metadata.paths.root).join("device-lease.token");
    if let Err(error) = write_text_atomic(&token_path, &lease.token) {
        let _ = fs::remove_file(&local_path);
        if read_json::<DeviceLease>(&global_path).is_ok_and(|current| current.token == lease.token)
        {
            let _ = fs::remove_file(&global_path);
        }
        return Err(error).context("写入设备租约令牌失败；已回滚本次租约");
    }
    Ok(lease)
}

fn validate_device_lease_global(state_root: &Path, lease: &DeviceLease) -> Result<()> {
    let path = device_lease_path(state_root, &lease.serial);
    let current = read_json::<DeviceLease>(&path).context("全局设备租约缺失")?;
    if current.token != lease.token || current.runtime_id != lease.runtime_id {
        bail!("设备租约已被其他 Worktree 替换");
    }
    Ok(())
}

fn remove_device_lease(state_root: &Path, lease: &DeviceLease) -> Result<()> {
    let global_path = device_lease_path(state_root, &lease.serial);
    let lock_path = global_path.with_extension("lock");
    recover_state_lock_if_stale(&lock_path)?;
    let _guard = StateFileLock::acquire(&lock_path)?;
    let current = match read_json::<DeviceLease>(&global_path) {
        Ok(current) => current,
        Err(error) if !global_path.exists() => return Err(error).context("全局设备租约已经缺失"),
        Err(error) => return Err(error).context("读取全局设备租约失败"),
    };
    if current.token != lease.token || current.runtime_id != lease.runtime_id {
        bail!("设备租约已由其他持有者更新，拒绝释放");
    }
    fs::remove_file(global_path).context("删除全局设备租约失败")
}

fn spawn_lease_heartbeat(
    state_root: PathBuf,
    metadata: Metadata,
    lease: DeviceLease,
    keep_alive: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while keep_alive.load(Ordering::SeqCst) {
            for _ in 0..20 {
                if !keep_alive.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(StdDuration::from_millis(250));
            }
            if renew_device_lease(&state_root, &metadata, &lease).is_err() {
                failed.store(true, Ordering::SeqCst);
                return;
            }
        }
    })
}

fn renew_device_lease(state_root: &Path, metadata: &Metadata, lease: &DeviceLease) -> Result<()> {
    let worktree_gate = PathBuf::from(&metadata.paths.worktree_root).join("locks/build-gate.lock");
    let _worktree_guard = StateFileLock::acquire(&worktree_gate)?;
    let runtime_gate = PathBuf::from(&metadata.paths.root).join("locks/device-acquire.lock");
    let _runtime_guard = StateFileLock::acquire(&runtime_gate)?;
    let global_path = device_lease_path(state_root, &lease.serial);
    let local_path = PathBuf::from(&metadata.paths.root).join("device-lease.json");
    let _guard = StateFileLock::acquire(&global_path.with_extension("lock"))?;
    let mut current = read_json::<DeviceLease>(&global_path).context("续租时读取全局租约失败")?;
    if current.token != lease.token || current.runtime_id != metadata.runtime_id {
        bail!("设备租约已被替换，拒绝续期");
    }
    let now = Utc::now();
    current.heartbeat_at = now.to_rfc3339();
    current.expires_at =
        (now + Duration::seconds(current.lease_seconds.min(i64::MAX as u64) as i64)).to_rfc3339();
    write_json_atomic(&global_path, &current)?;
    write_json_atomic(&local_path, &current)
}

fn acquire_device_operation(
    runtime: &Runtime,
    metadata: &Metadata,
    lease: &DeviceLease,
) -> Result<String> {
    let path = PathBuf::from(&metadata.paths.root).join("locks/device-operation.json");
    let now = Utc::now();
    let token = unique_token();
    let worktree_gate = PathBuf::from(&metadata.paths.worktree_root).join("locks/build-gate.lock");
    recover_state_lock_if_stale(&worktree_gate)?;
    let _build_gate = StateFileLock::acquire(&worktree_gate)?;
    validate_device_lease_global(&runtime.state_root, lease)?;
    let lock = TestLease {
        schema_version: SCHEMA_VERSION,
        runtime_id: metadata.runtime_id.clone(),
        agent_id: metadata.agent_id.clone(),
        serial: lease.serial.clone(),
        pid: std::process::id(),
        process_start_time: process_start_time(),
        token: token.clone(),
        acquired_at: now.to_rfc3339(),
        expires_at: (now + Duration::hours(24)).to_rfc3339(),
        heartbeat_at: now.to_rfc3339(),
    };
    let mut temporary = NamedTempFile::new_in(path.parent().expect("设备锁目录"))?;
    temporary.write_all(serde_json::to_string_pretty(&lock)?.as_bytes())?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => Ok(token),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            match read_json::<TestLease>(&path) {
                Ok(current)
                    if !process_identity_alive(current.pid, &current.process_start_time) =>
                {
                    let contents = fs::read_to_string(&path).unwrap_or_default();
                    if read_json::<TestLease>(&path)
                        .is_ok_and(|latest| latest.token == current.token)
                        && fs::read_to_string(&path).is_ok_and(|latest| latest == contents)
                    {
                        fs::remove_file(&path)?;
                        drop(_build_gate);
                        acquire_device_operation(runtime, metadata, lease)
                    } else {
                        bail!("设备操作锁在回收期间发生变化");
                    }
                }
                Ok(current) => bail!(
                    "设备 {} 已由 Worktree {} 的 PID {} 操作",
                    current.serial,
                    current.runtime_id,
                    current.pid
                ),
                Err(_) => bail!("设备操作锁内容无效，拒绝抢占：{}", path.display()),
            }
        }
        Err(error) => Err(error.error).context("创建设备操作锁失败"),
    }
}

fn release_device_operation(runtime: &Runtime, metadata: &Metadata, token: &str) -> Result<()> {
    let path = PathBuf::from(&metadata.paths.root).join("locks/device-operation.json");
    let current = read_json::<TestLease>(&path).context("读取设备操作锁失败")?;
    if current.token != token || current.runtime_id != metadata.runtime_id {
        bail!("设备操作锁令牌不匹配，拒绝释放");
    }
    let latest = read_json::<TestLease>(&path).context("再次读取设备操作锁失败")?;
    if latest.token != token {
        bail!("设备操作锁已变化，拒绝释放");
    }
    fs::remove_file(path).context("删除设备操作锁失败")?;
    record_event(
        &runtime.state_root,
        "device_operation_released",
        serde_json::json!({"runtime_id": metadata.runtime_id, "serial": current.serial}),
    )
}

fn record_event(state_root: &Path, event: &str, details: serde_json::Value) -> Result<()> {
    fs::create_dir_all(state_root)?;
    let path = state_root.join("events.jsonl");
    let _guard = StateFileLock::acquire(&path.with_extension("lock"))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(
        file,
        "{}",
        serde_json::json!({"event": event, "at": Utc::now().to_rfc3339(), "details": details})
    )?;
    file.sync_all()?;
    Ok(())
}

fn is_tcp_port_bound(port: u16) -> bool {
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            drop(listener);
            false
        }
        Err(_) => true,
    }
}

fn recover_state_lock_if_stale(path: &Path) -> Result<()> {
    let modified = match fs::metadata(path).and_then(|metadata| metadata.modified()) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("读取状态锁时间失败"),
    };
    if modified.elapsed().unwrap_or_default() < StdDuration::from_secs(30) {
        return Ok(());
    }
    let contents = fs::read_to_string(path).unwrap_or_default();
    let mut lines = contents.lines();
    let pid = lines.next().and_then(|value| value.parse::<u32>().ok());
    let start = lines.next();
    let token = lines.next();
    let (Some(pid), Some(start), Some(token)) = (pid, start, token) else {
        bail!("状态锁内容无效，拒绝自动回收：{}", path.display());
    };
    if token.trim().is_empty() || start.trim().is_empty() {
        bail!("状态锁缺少进程身份或令牌，拒绝自动回收：{}", path.display());
    }
    let stale = !process_identity_alive(pid, start);
    if stale {
        let latest = fs::read_to_string(path).unwrap_or_default();
        if latest == contents {
            fs::remove_file(path)
                .with_context(|| format!("删除已失效状态锁失败：{}", path.display()))?;
        }
    }
    Ok(())
}

fn release_ports(state_root: &Path, runtime_id: &str, agent_id: &str) -> Result<()> {
    let path = state_root.join("ports.json");
    if !path.exists() {
        return Ok(());
    }
    let lock_path = path.with_extension("lock");
    recover_state_lock_if_stale(&lock_path)?;
    let _guard = StateFileLock::acquire(&lock_path)?;
    let mut entries = load_port_registry(&path)?;
    entries.retain(|entry| entry.runtime_id != runtime_id || entry.owner != agent_id);
    write_json_atomic(&path, &entries)?;
    let local_path = state_root
        .join("agents")
        .join(safe_component(agent_id))
        .join(runtime_id)
        .join("ports.json");
    if local_path.exists() {
        write_json_atomic(&local_path, &Vec::<PortLease>::new())?;
    }
    Ok(())
}

fn refresh_port_leases(registry_path: &Path, metadata: &Metadata) -> Result<()> {
    if !registry_path.exists() {
        return Ok(());
    }
    let lock_path = registry_path.with_extension("lock");
    let _guard = StateFileLock::acquire(&lock_path)?;
    let mut entries = load_port_registry(registry_path)?;
    entries.retain(|entry| {
        !port_expired(entry)
            && worktree_is_active(
                registry_path.parent().unwrap_or(Path::new(".")),
                &entry.runtime_id,
            )
    });
    let expiry = (Utc::now() + Duration::days(7)).to_rfc3339();
    for entry in entries
        .iter_mut()
        .filter(|entry| entry.runtime_id == metadata.runtime_id && entry.owner == metadata.agent_id)
    {
        entry.expires_at = expiry.clone();
        entry.status = "active".to_string();
    }
    write_json_atomic(registry_path, &entries)?;
    let local = entries
        .iter()
        .filter(|entry| entry.runtime_id == metadata.runtime_id && entry.owner == metadata.agent_id)
        .cloned()
        .collect::<Vec<_>>();
    write_json_atomic(
        &PathBuf::from(&metadata.paths.root).join("ports.json"),
        &local,
    )
}

fn port_expired(entry: &PortLease) -> bool {
    entry.status != "active" || lock_expired_fields(&entry.expires_at)
}

fn worktree_is_active(state_root: &Path, runtime_id: &str) -> bool {
    read_json::<serde_json::Value>(
        &state_root
            .join("worktrees")
            .join(runtime_id)
            .join("worktree.json"),
    )
    .is_ok_and(|record| record.get("status").and_then(|status| status.as_str()) == Some("active"))
}

fn load_port_registry(path: &Path) -> Result<Vec<PortLease>> {
    match read_json(path) {
        Ok(entries) => Ok(entries),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(error).context("端口注册表无效；拒绝覆盖可能仍有效的登记"),
    }
}

fn valid_heap_size(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    let split = value.len().saturating_sub(1);
    let (number, suffix) = value.split_at(split);
    !number.is_empty() && number.parse::<u32>().is_ok_and(|n| n > 0) && matches!(suffix, "m" | "g")
}

fn device_lease_path(state_root: &Path, serial: &str) -> PathBuf {
    let digest = Sha256::digest(serial.as_bytes());
    let file_name = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    state_root.join("devices").join(format!("{file_name}.json"))
}

fn android_sdk_path() -> Option<PathBuf> {
    env::var_os("ANDROID_SDK_ROOT")
        .or_else(|| env::var_os("ANDROID_HOME"))
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| {
            env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .map(|path| path.join("Android/Sdk"))
                .filter(|path| path.is_dir())
        })
}

fn windows_terminal_wt_conflict() -> bool {
    if !cfg!(windows) {
        return false;
    }
    let output = match Command::new("where.exe").arg("wt").output() {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };
    let paths = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    terminal_wt_is_first(&paths)
}

fn terminal_wt_is_first(paths: &[String]) -> bool {
    paths.first().is_some_and(|path| {
        path.replace('/', "\\")
            .to_ascii_lowercase()
            .ends_with("\\windowsapps\\wt.exe")
    })
}

fn worktree_id(git_dir: &Path) -> String {
    let canonical = fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf());
    let digest = Sha256::digest(canonical.to_string_lossy().to_lowercase().as_bytes());
    digest[..10]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn fixed_version(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || b"._-".contains(&ch))
        && !value.to_ascii_uppercase().contains("SNAPSHOT")
        && !value.starts_with("latest.")
}

fn validate_artifact_version(artifact: &ArtifactEvidence, version: &str) -> Result<()> {
    // 未发布草稿可以没有制品；一旦附带制品，就必须提供完整且相互一致的证据。
    if artifact.url.is_none() && artifact.sha256.is_none() && artifact.coordinate.is_none() {
        return Ok(());
    }
    let (Some(url), Some(sha), Some(coordinate)) =
        (&artifact.url, &artifact.sha256, &artifact.coordinate)
    else {
        bail!("制品证据必须同时提供 URL、SHA-256 和 Maven 坐标");
    };
    let parts: Vec<_> = coordinate.split(':').collect();
    if parts.len() != 3
        || parts.iter().any(|part| part.is_empty())
        || !parts[..2].iter().all(|part| {
            part.bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || b"._-".contains(&ch))
        })
        || !fixed_version(parts[2])
        || parts[2] != version
    {
        bail!(
            "制品坐标必须为 group:artifact:{version}；请使用 candidate-version 的完整输出发布候选"
        );
    }
    if sha.len() != 64 || !sha.bytes().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("制品 SHA-256 必须是 64 位十六进制字符串");
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        bail!("制品地址必须是 http 或 https URL");
    }
    Ok(())
}

fn valid_agent_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_event_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn copy_cache_tree(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        bail!("缓存快照包含符号链接，拒绝导入：{}", source.display());
    }
    if metadata.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_text = name.to_string_lossy();
            if name_text.ends_with(".lock")
                || name_text.ends_with(".lck")
                || name_text == "gc.properties"
            {
                continue;
            }
            copy_cache_tree(&entry.path(), &target.join(name))?;
        }
    } else if metadata.is_file() {
        fs::copy(source, target)?;
    } else {
        bail!("缓存快照包含非普通文件，拒绝导入：{}", source.display());
    }
    Ok(())
}

fn unique_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("操作系统随机数生成器不可用");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_gradle_args(args: &[String], allow_clean: bool) -> Result<()> {
    validate_gradle_isolation_args(args)?;
    let has_clean = args
        .iter()
        .any(|argument| argument == "clean" || argument.ends_with(":clean"));
    if has_clean && !allow_clean {
        bail!("默认禁止 clean；确认需要时传入 --allow-clean");
    }
    if args
        .iter()
        .any(|argument| argument == "--stop" || argument == "--daemon")
    {
        bail!(
            "agentctl 不接受 --stop 或 --daemon 参数；Gradle 守护进程由独立 GRADLE_USER_HOME 隔离管理"
        );
    }
    Ok(())
}

fn validate_gradle_isolation_args(args: &[String]) -> Result<()> {
    for argument in args {
        let key = argument.split('=').next().unwrap_or(argument);
        let redirects = [
            "--gradle-user-home",
            "--project-dir",
            "--project-cache-dir",
            "--build-file",
            "--settings-file",
            "--include-build",
        ];
        let long_redirect = key.starts_with("--")
            && key.len() > 2
            && redirects.iter().any(|option| option.starts_with(key));
        let short_redirect = ["-g", "-p", "-b", "-c"]
            .iter()
            .any(|option| argument.starts_with(option) && !argument.starts_with("--"));
        if long_redirect || short_redirect || argument.starts_with("-Dgradle.user.home") {
            bail!(
                "禁止重定向 Gradle 用户目录、项目目录或构建入口；请在目标 Worktree 中运行 agentctl"
            );
        }
    }
    Ok(())
}

fn android_test_gradle_args(args: &[String]) -> Vec<String> {
    let mut result = args.to_vec();
    if !result.iter().any(|argument| argument == "--no-daemon") {
        result.push("--no-daemon".to_string());
    }
    result
}

fn powershell_invocation(wrapper: &Path, args: &[String]) -> String {
    let mut script = format!("& {}", ps_quote(&wrapper.display().to_string()));
    for argument in args {
        script.push(' ');
        script.push_str(&ps_quote(argument));
    }
    script.push_str("; exit $LASTEXITCODE");
    let encoded: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&encoded)
}

#[cfg(not(windows))]
fn powershell_invocation(_wrapper: &Path, _args: &[String]) -> String {
    String::new()
}

fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        result.push(TABLE[(a >> 2) as usize] as char);
        result.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        result.push(if chunk.len() > 1 {
            TABLE[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            TABLE[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    result
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .with_context(|| format!("启动 git {} 失败", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} 失败，退出码 {:?}",
            args.join(" "),
            output.status.code()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let ignored = [".git", ".gradle", "build", ".cxx", "target"];
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if ignored.iter().any(|item| name == *item) {
            continue;
        }
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn command_exists(name: &str) -> bool {
    let locator = if cfg!(windows) { "where.exe" } else { "which" };
    Command::new(locator)
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn worktrunk_available() -> bool {
    if !cfg!(windows) {
        return command_exists("wt");
    }
    Command::new("where.exe")
        .arg("wt")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.to_ascii_lowercase().contains("worktrunk"))
        })
}

fn run_managed_output(
    command: &mut Command,
    build_lock_path: &Path,
    build_lock_token: &str,
) -> Result<Output> {
    let limits = configured_process_limits()?;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("启动受控子进程失败")?;
    let managed = match ManagedChild::attach(&child, &limits) {
        Ok(managed) => managed,
        Err(error) => {
            let stop_error = terminate_child_tree(&mut child).err();
            return match stop_error {
                Some(stop_error) => Err(error.context(format!(
                    "受控启动失败，且不能确认子进程树已回收：{stop_error:#}"
                ))),
                None => Err(error),
            };
        }
    };
    if let Err(error) = update_build_lock_child(build_lock_path, build_lock_token, child.id()) {
        drop(managed);
        let _ = child.wait();
        return Err(error);
    }
    let output = child.wait_with_output().context("等待受控子进程失败");
    let clear_result = update_build_lock_child(build_lock_path, build_lock_token, 0);
    drop(managed);
    match (output, clear_result) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error).context("子进程已结束但清理构建锁子进程记录失败"),
        (Err(error), Err(lock_error)) => {
            Err(error.context(format!("并且清理构建锁子进程记录失败：{lock_error:#}")))
        }
    }
}

fn run_managed_streaming(
    command: &mut Command,
    build_lock_path: &Path,
    build_lock_token: &str,
    log_base: &Path,
    stdout_to_stderr: bool,
) -> Result<std::process::ExitStatus> {
    let limits = configured_process_limits()?;
    let timeout = managed_timeout()?;
    let started = Instant::now();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("启动受控子进程失败")?;
    let managed = match ManagedChild::attach(&child, &limits) {
        Ok(managed) => managed,
        Err(error) => {
            let stop_error = terminate_child_tree(&mut child).err();
            return match stop_error {
                Some(stop_error) => Err(error.context(format!(
                    "受控启动失败，且不能确认子进程树已回收：{stop_error:#}"
                ))),
                None => Err(error),
            };
        }
    };
    if let Err(error) = update_build_lock_child(build_lock_path, build_lock_token, child.id()) {
        drop(managed);
        let _ = child.wait();
        return Err(error);
    }
    let stdout = child.stdout.take().expect("已为子进程配置标准输出管道");
    let stderr = child.stderr.take().expect("已为子进程配置标准错误管道");
    let stdout_path = log_base.with_extension("stdout.log");
    let stderr_path = log_base.with_extension("stderr.log");
    let stdout_thread = thread::spawn(move || -> Result<()> {
        let mut source = stdout;
        let mut log_error = None;
        let mut log = match File::create(&stdout_path) {
            Ok(log) => Some(log),
            Err(error) => {
                log_error = Some(error);
                None
            }
        };
        let mut buffer = [0; 16 * 1024];
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            // JSON 模式只把结果写入标准输出；子进程原始日志仍按来源独立保存。
            if stdout_to_stderr {
                let _ = std::io::stderr().write_all(&buffer[..count]);
            } else {
                let _ = std::io::stdout().write_all(&buffer[..count]);
            }
            if let Some(file) = log.as_mut() {
                if let Err(error) = file.write_all(&buffer[..count]) {
                    log_error = Some(error);
                    log = None;
                }
            }
        }
        if let Some(file) = log.as_ref() {
            if let Err(error) = file.sync_all() {
                log_error = Some(error);
            }
        }
        match log_error {
            Some(error) => Err(error).context("保存 ADB 标准输出日志失败"),
            None => Ok(()),
        }
    });
    let stderr_thread = thread::spawn(move || -> Result<()> {
        let mut source = stderr;
        let mut log_error = None;
        let mut log = match File::create(&stderr_path) {
            Ok(log) => Some(log),
            Err(error) => {
                log_error = Some(error);
                None
            }
        };
        let mut buffer = [0; 16 * 1024];
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let _ = std::io::stderr().write_all(&buffer[..count]);
            if let Some(file) = log.as_mut() {
                if let Err(error) = file.write_all(&buffer[..count]) {
                    log_error = Some(error);
                    log = None;
                }
            }
        }
        if let Some(file) = log.as_ref() {
            if let Err(error) = file.sync_all() {
                log_error = Some(error);
            }
        }
        match log_error {
            Some(error) => Err(error).context("保存 ADB 标准错误日志失败"),
            None => Ok(()),
        }
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(error) => break Err(error).context("等待受控子进程失败"),
            Ok(None) => {}
        }
        if started.elapsed() >= timeout {
            eprintln!(
                "受控进程超过 {} 秒限制，终止本次子进程树并保留失败日志",
                timeout.as_secs()
            );
            if let Err(error) = terminate_child_tree(&mut child) {
                break Err(error);
            }
            break child.wait().context("回收超时子进程失败");
        }
        thread::sleep(StdDuration::from_millis(100));
    };
    // 关闭任务对象，避免孙进程继承输出管道后使日志收集永久等待。
    drop(managed);
    let stdout_result = stdout_thread
        .join()
        .map_err(|_| anyhow::anyhow!("收集 ADB 标准输出的线程异常退出"))?;
    let stderr_result = stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("收集 ADB 标准错误的线程异常退出"))?;
    let clear_result = update_build_lock_child(build_lock_path, build_lock_token, 0);
    stdout_result.context("保存或转发 ADB 标准输出失败")?;
    stderr_result.context("保存或转发 ADB 标准错误失败")?;
    match (status, clear_result) {
        (Ok(status), Ok(())) => Ok(status),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error).context("ADB 命令结束但清理构建锁子进程记录失败"),
        (Err(error), Err(lock_error)) => {
            Err(error.context(format!("并且清理构建锁子进程记录失败：{lock_error:#}")))
        }
    }
}

fn managed_timeout() -> Result<StdDuration> {
    let seconds = env::var("AGENT_COMMAND_TIMEOUT_SECONDS").unwrap_or_else(|_| "7200".into());
    let seconds = seconds
        .parse::<u64>()
        .ok()
        .filter(|value| (1..=86400).contains(value))
        .ok_or_else(|| anyhow::anyhow!("AGENT_COMMAND_TIMEOUT_SECONDS 必须是 1 到 86400 的秒数"))?;
    Ok(StdDuration::from_secs(seconds))
}

fn terminate_child_tree(child: &mut std::process::Child) -> Result<()> {
    #[cfg(windows)]
    {
        let status = Command::new("taskkill.exe")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("启动 taskkill 回收 Gradle Wrapper 进程树失败")?;
        if !status.success() {
            let _ = child.kill();
            let _ = child.wait();
            bail!("taskkill 无法确认 Gradle Wrapper 子进程树已回收");
        }
        let _ = child.wait();
        Ok(())
    }
    #[cfg(not(windows))]
    {
        child.kill().context("终止受控构建进程失败")?;
        child.wait().context("回收受控构建进程失败")?;
        Ok(())
    }
}

fn configured_process_limits() -> Result<ProcessLimits> {
    let cpu_percent = env::var("AGENT_CPU_PERCENT")
        .ok()
        .map(|value| {
            value
                .parse::<u32>()
                .ok()
                .filter(|rate| (1..=100).contains(rate))
                .ok_or_else(|| anyhow::anyhow!("AGENT_CPU_PERCENT 必须是 1 到 100 的整数"))
        })
        .transpose()?
        .unwrap_or(100);
    let memory_bytes = env::var("AGENT_MEMORY_LIMIT_MB")
        .ok()
        .map(|value| {
            let megabytes = value
                .parse::<usize>()
                .ok()
                .filter(|limit| *limit >= 256)
                .ok_or_else(|| anyhow::anyhow!("AGENT_MEMORY_LIMIT_MB 必须是不小于 256 的整数"))?;
            megabytes
                .checked_mul(1024 * 1024)
                .ok_or_else(|| anyhow::anyhow!("AGENT_MEMORY_LIMIT_MB 超出可用范围"))
        })
        .transpose()?;
    if !cfg!(windows) && (env::var_os("AGENT_CPU_PERCENT").is_some() || memory_bytes.is_some()) {
        bail!("当前平台尚不支持 AGENT_CPU_PERCENT 或 AGENT_MEMORY_LIMIT_MB 硬限制");
    }
    Ok(ProcessLimits {
        cpu_percent,
        memory_bytes,
    })
}

// 会话显式固定 JDK 时直接启动 Wrapper，避免批处理和 PATH 选中另一套 Java。
fn windows_gradle_command(wrapper: &Path, args: &[String]) -> Result<Command> {
    if let Some(home) = env::var_os("AGENT_JAVA_HOME") {
        return jdk17_wrapper_command(Path::new(&home), wrapper, args);
    }
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-EncodedCommand",
        &powershell_invocation(wrapper, args),
    ]);
    Ok(command)
}

fn jdk17_wrapper_command(home: &Path, wrapper: &Path, args: &[String]) -> Result<Command> {
    if !home.is_absolute() {
        bail!("AGENT_JAVA_HOME 必须为固定 JDK 17 的绝对路径");
    }
    let release =
        fs::read_to_string(home.join("release")).context("无法读取固定 JDK 的版本声明")?;
    let version = release
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))
        .map(|value| value.trim_matches('"'));
    if !version.is_some_and(|value| value == "17" || value.starts_with("17.")) {
        bail!("AGENT_JAVA_HOME 指向的 JDK 不是 17，拒绝启动构建");
    }
    let java = home.join("bin/java.exe");
    let jar = wrapper
        .parent()
        .context("Wrapper 缺少所属目录")?
        .join("gradle/wrapper/gradle-wrapper.jar");
    if !java.is_file() || !jar.is_file() {
        bail!("固定 JDK 的 java.exe 或当前项目的 Wrapper JAR 不存在");
    }
    let mut command = Command::new(java);
    command
        .env("JAVA_HOME", home)
        .args(["-Xmx64m", "-Dfile.encoding=UTF-8", "-classpath"])
        .arg(jar)
        .arg("org.gradle.wrapper.GradleWrapperMain")
        .args(args);
    Ok(command)
}

#[cfg(test)]
mod pinned_java_tests {
    use super::*;

    #[test]
    fn direct_wrapper_keeps_arguments_and_rejects_wrong_jdk() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("jdk with spaces");
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::write(home.join("bin/java.exe"), b"test only").unwrap();
        fs::write(home.join("release"), "JAVA_VERSION=\"17.0.16\"\n").unwrap();
        let project = root.path().join("project");
        fs::create_dir_all(project.join("gradle/wrapper")).unwrap();
        fs::write(
            project.join("gradle/wrapper/gradle-wrapper.jar"),
            b"test only",
        )
        .unwrap();
        let wrapper = project.join("gradlew.bat");
        let args = vec![
            ":app:testDebugUnitTest".into(),
            "-Pvalue=a b;literal".into(),
        ];
        let command = jdk17_wrapper_command(&home, &wrapper, &args).unwrap();
        assert_eq!(command.get_program(), home.join("bin/java.exe"));
        let actual: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&actual[actual.len() - 2..], &args);
        assert!(actual.contains(&"org.gradle.wrapper.GradleWrapperMain".into()));
        fs::write(home.join("release"), "JAVA_VERSION=\"21.0.1\"\n").unwrap();
        assert!(jdk17_wrapper_command(&home, &wrapper, &args).is_err());
        assert!(jdk17_wrapper_command(Path::new("relative"), &wrapper, &args).is_err());
    }
}

fn update_build_lock_child(path: &Path, token: &str, pid: u32) -> Result<()> {
    let guard_path = path.with_file_name("build-gate.lock");
    let _guard = StateFileLock::acquire(&guard_path)?;
    let mut lock = read_json::<BuildLock>(path).context("更新子进程记录时读取构建锁失败")?;
    if lock.token != token {
        bail!("更新子进程记录时构建锁令牌不匹配");
    }
    lock.gradle_pid = (pid != 0).then_some(pid);
    lock.gradle_process_start_time = (pid != 0).then(|| process_start_time_for_pid(pid));
    lock.heartbeat_at = Utc::now().to_rfc3339();
    write_json_atomic(path, &lock)
}

fn read_build_lock_consistently(path: &Path) -> Result<BuildLock> {
    let gate_path = path.with_file_name("build-gate.lock");
    let _gate = StateFileLock::acquire(&gate_path)?;
    read_json::<BuildLock>(path)
}

fn stop_gradle_daemons(cwd: &Path, gradle_user_home: &str) -> Result<()> {
    let cwd = normalized_process_path(cwd);
    let wrapper = if cfg!(windows) {
        cwd.join("gradlew.bat")
    } else {
        cwd.join("gradlew")
    };
    if !wrapper.is_file() {
        return Ok(());
    }
    let status = if cfg!(windows) {
        let script = format!(
            "& {} --stop; exit $LASTEXITCODE",
            ps_quote(&wrapper.display().to_string())
        );
        let encoded: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                &base64(&encoded),
            ])
            .current_dir(cwd)
            .env("GRADLE_USER_HOME", gradle_user_home)
            .status()
    } else {
        Command::new(&wrapper)
            .arg("--stop")
            .current_dir(cwd)
            .env("GRADLE_USER_HOME", gradle_user_home)
            .status()
    }
    .context("停止当前 Worktree 的 Gradle 守护进程失败")?;
    if !status.success() {
        bail!("Gradle --stop 失败，未删除缓存以避免损坏运行中的缓存");
    }
    Ok(())
}

fn normalized_process_path(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    let value = value.strip_prefix("\\\\?\\").unwrap_or(&value);
    PathBuf::from(value)
}

fn status_check(name: &str, pass: bool) -> serde_json::Value {
    serde_json::json!({"name":name,"status":if pass {"PASS"} else {"FAIL"}})
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(path).with_context(
        || format!("读取状态文件失败：{}", path.display()),
    )?)?)
}

fn require_clean_worktree(cwd: &Path) -> Result<()> {
    if !git_output(cwd, &["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
        bail!("消费者工作区存在未提交改动；提交迁移后再开始候选验证");
    }
    Ok(())
}

fn validate_attempt_owner(previous: Option<&serde_json::Value>, metadata: &Metadata) -> Result<()> {
    let previous = previous.ok_or_else(|| anyhow::anyhow!("必须先开始 validation_started"))?;
    if previous["status"] != "validation_started" {
        bail!("必须先开始 validation_started");
    }
    if previous["agent_id"] != metadata.agent_id || previous["worktree_id"] != metadata.worktree_id
    {
        bail!("验证属于其他 Agent 或 Worktree，拒绝覆盖");
    }
    if previous["expires_at"]
        .as_str()
        .is_none_or(lock_expired_fields)
    {
        bail!("验证租约已过期；必须重新开始验证");
    }
    Ok(())
}

fn validate_candidate_receipt(
    receipt: &BuildReceipt,
    metadata: &Metadata,
    event: &serde_json::Value,
    attempt: &serde_json::Value,
    consumer: &str,
) -> Result<()> {
    let target = routing::require_consumer_repository(event, consumer, &metadata.repository)?;
    if !receipt.success || receipt.exit_code != Some(0) || !receipt.worktree_clean {
        bail!("构建未成功或源码在验证期间存在未提交改动");
    }
    if receipt.agent_id != metadata.agent_id
        || receipt.worktree_id != metadata.worktree_id
        || receipt.runtime_id != metadata.runtime_id
        || receipt.repository != metadata.repository
        || receipt.worktree_path != metadata.worktree_path
        || receipt.commit != metadata.commit
        || attempt["commit"] != receipt.commit
    {
        bail!("构建收据不属于当前 Agent、Worktree 或消费者提交");
    }
    let validation = receipt.validation.as_ref().ok_or_else(|| {
        anyhow::anyhow!("缺少 Gradle 实际制品解析和测试执行证据，不能确认 passed")
    })?;
    if validation.schema_version != 1
        || validation.run_id.is_empty()
        || validation.attempt_id.is_empty()
        || attempt["attempt_id"] != validation.attempt_id
        || validation.consumer != consumer
        || event["event_id"] != validation.event_id
        || receipt.event_id.as_deref() != Some(&validation.event_id)
    {
        bail!("验证证据未绑定本次验证、事件和消费者");
    }
    let expected = event["artifact"]["sha256"].as_str().unwrap_or_default();
    if expected.len() != 64
        || !expected.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !validation.artifact_sha256.eq_ignore_ascii_case(expected)
        || receipt.artifact_sha256.as_deref() != Some(&validation.artifact_sha256)
        || event["artifact"]["coordinate"] != validation.coordinate
    {
        bail!("实际解析的候选坐标或摘要与事件不一致");
    }
    if validation.project != target.project
        || validation.configuration.is_empty()
        || !routing::task_belongs_to_project(&validation.project, &validation.task)
        || !receipt.command.contains(&validation.task)
        || validation.tests <= validation.skipped
        || validation.failures != 0
    {
        bail!("缺少当前消费者实际执行且通过的测试");
    }
    let started = DateTime::parse_from_rfc3339(&receipt.started_at)?;
    let finished = DateTime::parse_from_rfc3339(&receipt.finished_at)?;
    let attempt_started = DateTime::parse_from_rfc3339(
        attempt["validation_started_at"]
            .as_str()
            .unwrap_or_default(),
    )?;
    let expires = DateTime::parse_from_rfc3339(attempt["expires_at"].as_str().unwrap_or_default())?;
    if started < attempt_started
        || finished < started
        || finished > expires
        || finished > Utc::now()
    {
        bail!("构建收据不在本次验证的有效时间范围内");
    }
    Ok(())
}

fn verified_ack_status(ack: &serde_json::Value) -> &str {
    let status = ack["status"].as_str().unwrap_or("pending");
    if status == "passed"
        && (ack["evidence_version"] != 2 || ack["receipt_snapshot"]["validation"].is_null())
    {
        "needs_revalidation"
    } else {
        status
    }
}

fn scoped_ack_status<'a>(
    ack: &'a serde_json::Value,
    event: &serde_json::Value,
    consumer: &str,
) -> &'a str {
    let status = verified_ack_status(ack);
    let invalid_pass = status == "passed" && !integration::passed_ack(ack, event, consumer);
    let invalid_scope = status != "passed"
        && status != "needs_revalidation"
        && (ack["event_id"] != event["event_id"]
            || ack["consumer"] != consumer
            || routing::require_consumer_repository(
                event,
                consumer,
                ack["repository"].as_str().unwrap_or_default(),
            )
            .is_err()
            || routing::consumer_target(event, consumer)
                .is_ok_and(|target| ack["consumer_project"] != target.project));
    let expired = status == "validation_started"
        && ack["expires_at"].as_str().is_none_or(lock_expired_fields);
    if invalid_pass || invalid_scope || expired {
        "needs_revalidation"
    } else {
        status
    }
}

fn symbols_from_report(report: &serde_json::Value) -> Result<std::collections::BTreeSet<String>> {
    if report["snapshot_format"].as_str() != Some("kotlin-production-declarations-v2") {
        bail!("契约基线格式不兼容；请使用当前 agentctl 从基线提交重新生成快照");
    }
    report
        .get("symbols")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("契约基线缺少 symbols 数组"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("契约基线包含非字符串符号"))
        })
        .collect()
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_text_atomic(path, &serde_json::to_string_pretty(value)?)
}

fn write_text_atomic(path: &Path, value: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("目标文件没有父目录"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(value.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("原子写入失败：{}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::FileTimes;
    use std::time::SystemTime;
    use tempfile::tempdir;

    fn git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {:?} failed", args);
    }

    fn validation_fixture() -> (Metadata, BuildReceipt, serde_json::Value, serde_json::Value) {
        let now = Utc::now();
        let metadata: Metadata = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "runtime_id": "runtime-a", "agent_id": "agent-a",
            "worktree_id": "tree-a", "repository": "repo", "worktree_path": "tree",
            "branch": "candidate", "commit": "consumer-commit", "is_primary": false,
            "created_at": now.to_rfc3339(), "last_seen_at": now.to_rfc3339(), "status": "active",
            "paths": {"root":"", "worktree_root":"", "gradle_user_home":"", "maven_local":"", "temp":"", "logs":""}
        })).unwrap();
        let sha = "a".repeat(64);
        let event = serde_json::json!({"event_id":"event-a", "repository":"repo", "affected_consumers":["consumer"], "artifact":{"coordinate":"g:a:1", "sha256":sha}});
        let attempt = serde_json::json!({
            "status":"validation_started", "attempt_id":"attempt-a", "agent_id":"agent-a",
            "worktree_id":"tree-a", "commit":"consumer-commit",
            "validation_started_at":(now - Duration::minutes(1)).to_rfc3339(),
            "expires_at":(now + Duration::hours(2)).to_rfc3339()
        });
        let receipt: BuildReceipt = serde_json::from_value(serde_json::json!({
            "schema_version":1, "runtime_id":"runtime-a", "agent_id":"agent-a", "worktree_id":"tree-a",
            "repository":"repo", "worktree_path":"tree", "commit":"consumer-commit", "event_id":"event-a",
            "artifact_sha256":sha, "worktree_clean":true, "command":[":consumer:test"],
            "started_at":(now - Duration::seconds(30)).to_rfc3339(), "finished_at":now.to_rfc3339(),
            "elapsed_ms":30000, "exit_code":0, "success":true, "stdout_log":"out", "stderr_log":"err",
            "validation": {"schema_version":1, "run_id":"run-a", "attempt_id":"attempt-a",
                "event_id":"event-a", "consumer":"consumer", "coordinate":"g:a:1", "artifact_sha256":sha,
                "project":":consumer", "configuration":"testRuntimeClasspath", "task":":consumer:test",
                "tests":2, "failures":0, "skipped":0}
        })).unwrap();
        (metadata, receipt, event, attempt)
    }

    #[test]
    fn candidate_receipt_rejects_help_stale_other_consumer_and_changed_checkout() {
        let (metadata, receipt, event, attempt) = validation_fixture();
        let valid = |value: &BuildReceipt| {
            validate_candidate_receipt(value, &metadata, &event, &attempt, "consumer")
        };
        assert!(valid(&receipt).is_ok());
        let mut bad = receipt.clone();
        bad.command = vec!["help".into()];
        bad.validation = None;
        assert!(valid(&bad).is_err());
        let mut bad = receipt.clone();
        bad.validation.as_mut().unwrap().attempt_id = "old-attempt".into();
        assert!(valid(&bad).is_err());
        let mut bad = receipt.clone();
        bad.validation.as_mut().unwrap().consumer = "other".into();
        assert!(valid(&bad).is_err());
        let mut bad = receipt.clone();
        bad.commit = "old-commit".into();
        assert!(valid(&bad).is_err());
        let mut bad = receipt.clone();
        bad.worktree_clean = false;
        assert!(valid(&bad).is_err());
        let mut bad = receipt.clone();
        bad.agent_id = "other-agent".into();
        assert!(valid(&bad).is_err());
        let mut bad = receipt.clone();
        bad.started_at = (Utc::now() - Duration::hours(1)).to_rfc3339();
        assert!(valid(&bad).is_err());
    }

    #[test]
    fn candidate_receipt_requires_matching_artifact_and_executed_tests() {
        let (metadata, receipt, event, attempt) = validation_fixture();
        let valid = |value: &BuildReceipt| {
            validate_candidate_receipt(value, &metadata, &event, &attempt, "consumer")
        };
        for (tests, skipped, failures) in [(0, 0, 0), (2, 2, 0), (2, 0, 1)] {
            let mut bad = receipt.clone();
            let evidence = bad.validation.as_mut().unwrap();
            evidence.tests = tests;
            evidence.skipped = skipped;
            evidence.failures = failures;
            assert!(valid(&bad).is_err());
        }
        let mut bad = receipt.clone();
        bad.validation.as_mut().unwrap().coordinate = "g:a:2".into();
        assert!(valid(&bad).is_err());
        let mut bad = receipt.clone();
        bad.validation.as_mut().unwrap().artifact_sha256 = "b".repeat(64);
        assert!(valid(&bad).is_err());
        let mut bad = receipt.clone();
        bad.command = vec![":other:test".into()];
        assert!(valid(&bad).is_err());
    }

    #[test]
    fn candidate_receipt_rejects_same_named_module_in_another_repository_or_project() {
        let (mut metadata, mut receipt, mut event, attempt) = validation_fixture();
        event["repository"] = serde_json::json!("another-repository");
        assert!(
            validate_candidate_receipt(&receipt, &metadata, &event, &attempt, "consumer").is_err()
        );
        let repository = std::env::current_dir().unwrap().display().to_string();
        metadata.repository = repository.clone();
        receipt.repository = repository.clone();
        event["repository"] = serde_json::json!(repository);
        event["routing_schema_version"] = serde_json::json!(1);
        event["consumer_targets"] = serde_json::json!({
            "consumer":{"repository":repository, "project":":nested:consumer"}
        });
        assert!(
            validate_candidate_receipt(&receipt, &metadata, &event, &attempt, "consumer").is_err()
        );
        event["consumer_targets"]["consumer"]["project"] = serde_json::json!(":consumer");
        assert!(
            validate_candidate_receipt(&receipt, &metadata, &event, &attempt, "consumer").is_ok()
        );
    }

    #[test]
    fn displayed_ack_status_rechecks_full_receipt_and_consumer_route() {
        let (_, receipt, mut event, mut ack) = validation_fixture();
        ack["event_id"] = event["event_id"].clone();
        ack["consumer"] = serde_json::json!("consumer");
        ack["status"] = serde_json::json!("passed");
        ack["evidence_version"] = serde_json::json!(2);
        ack["receipt_snapshot"] = serde_json::to_value(&receipt).unwrap();
        assert_eq!(scoped_ack_status(&ack, &event, "consumer"), "passed");
        event["repository"] = serde_json::json!("another-repository");
        assert_eq!(
            scoped_ack_status(&ack, &event, "consumer"),
            "needs_revalidation"
        );
        event["repository"] = serde_json::json!("repo");
        ack["receipt_snapshot"]["validation"]["tests"] = serde_json::json!(0);
        assert_eq!(
            scoped_ack_status(&ack, &event, "consumer"),
            "needs_revalidation"
        );
    }

    #[test]
    fn displayed_lease_status_requires_repository_scope_and_unexpired_lease() {
        let (_, _, event, mut ack) = validation_fixture();
        ack["event_id"] = event["event_id"].clone();
        ack["consumer"] = serde_json::json!("consumer");
        assert_eq!(
            scoped_ack_status(&ack, &event, "consumer"),
            "needs_revalidation"
        );
        ack["repository"] = serde_json::json!("repo");
        ack["consumer_project"] = serde_json::json!(":consumer");
        assert_eq!(
            scoped_ack_status(&ack, &event, "consumer"),
            "validation_started"
        );
        ack["repository"] = serde_json::json!("another-repo");
        assert_eq!(
            scoped_ack_status(&ack, &event, "consumer"),
            "needs_revalidation"
        );
        ack["repository"] = serde_json::json!("repo");
        ack["expires_at"] = serde_json::json!((Utc::now() - Duration::minutes(1)).to_rfc3339());
        assert_eq!(
            scoped_ack_status(&ack, &event, "consumer"),
            "needs_revalidation"
        );
    }

    #[test]
    fn candidate_receipt_accepts_root_tests_but_not_tests_in_a_child_project() {
        let (mut metadata, mut receipt, mut event, attempt) = validation_fixture();
        receipt.validation.as_mut().unwrap().task = ":consumer:child:test".into();
        receipt.command = vec![":consumer:child:test".into()];
        assert!(
            validate_candidate_receipt(&receipt, &metadata, &event, &attempt, "consumer").is_err()
        );
        let repository = std::env::current_dir().unwrap().display().to_string();
        metadata.repository = repository.clone();
        receipt.repository = repository.clone();
        event["repository"] = serde_json::json!(repository);
        event["routing_schema_version"] = serde_json::json!(1);
        event["consumer_targets"] =
            serde_json::json!({"consumer":{"repository":repository, "project":":"}});
        receipt.validation.as_mut().unwrap().project = ":".into();
        receipt.validation.as_mut().unwrap().task = ":test".into();
        receipt.command = vec![":test".into()];
        assert!(
            validate_candidate_receipt(&receipt, &metadata, &event, &attempt, "consumer").is_ok()
        );
    }

    #[test]
    fn validation_attempt_is_owned_and_expires_and_legacy_pass_needs_revalidation() {
        let (metadata, _, _, attempt) = validation_fixture();
        assert!(validate_attempt_owner(Some(&attempt), &metadata).is_ok());
        let mut other = metadata.clone();
        other.agent_id = "other".into();
        assert!(validate_attempt_owner(Some(&attempt), &other).is_err());
        let mut expired = attempt.clone();
        expired["expires_at"] = serde_json::json!((Utc::now() - Duration::seconds(1)).to_rfc3339());
        assert!(validate_attempt_owner(Some(&expired), &metadata).is_err());
        assert_eq!(
            verified_ack_status(&serde_json::json!({"status":"passed"})),
            "needs_revalidation"
        );
    }

    #[test]
    fn integration_manifest_rejects_non_string_events_instead_of_reporting_ready() {
        let temp = tempdir().unwrap();
        let runtime = Runtime::with_paths(temp.path().to_path_buf(), temp.path().join("state"));
        let manifest = temp.path().join("manifest.json");
        write_json_atomic(&manifest, &serde_json::json!({"events":[null, 12]})).unwrap();
        assert!(
            runtime
                .integration_status(manifest.to_str().unwrap(), true)
                .is_err()
        );
    }

    #[test]
    fn gradle_arguments_cannot_redirect_another_worktree_or_shared_cache() {
        for argument in [
            "-g",
            "-gF:/shared",
            "--gradle-user-home=F:/shared",
            "--gradle-user-h=F:/shared",
            "-p../other",
            "--project-cache-dir=F:/shared",
            "-bother.gradle",
            "--settings-file=other.gradle",
            "--include-build=../other",
            "-Dgradle.user.home=F:/shared",
        ] {
            assert!(
                validate_gradle_args(&[argument.into()], false).is_err(),
                "{argument}"
            );
        }
        assert!(
            validate_gradle_args(
                &[
                    ":library:test".into(),
                    "--configuration-cache".into(),
                    "-Iverify.gradle".into()
                ],
                false
            )
            .is_ok()
        );
    }

    #[test]
    fn terminal_collision_diagnostic_checks_actual_path_precedence() {
        let terminal = "C:/Users/test/AppData/Local/Microsoft/WindowsApps/wt.exe".to_string();
        let worktrunk = "C:/tools/worktrunk/wt.exe".to_string();
        assert!(terminal_wt_is_first(&[terminal.clone(), worktrunk.clone()]));
        assert!(terminal_wt_is_first(std::slice::from_ref(&terminal)));
        assert!(!terminal_wt_is_first(&[worktrunk, terminal]));
        assert!(!terminal_wt_is_first(&[]));
    }

    #[test]
    fn concurrent_event_import_cannot_replace_same_id_with_different_content() {
        let temp = tempdir().unwrap();
        let runtime = Runtime::with_paths(temp.path().to_path_buf(), temp.path().join("state"));
        let barrier = Arc::new(std::sync::Barrier::new(12));
        let mut workers = Vec::new();
        for index in 0..12 {
            let path = temp.path().join(format!("candidate-{index}.json"));
            write_json_atomic(&path, &serde_json::json!({
                "event_id":"same-candidate", "event_type":"contract_candidate",
                "affected_consumers":["consumer"], "candidate_version":format!("1.0.0-dev.{index}")
            })).unwrap();
            let runtime = runtime.clone();
            let barrier = barrier.clone();
            workers.push(thread::spawn(move || {
                barrier.wait();
                runtime.import_event(path.to_str().unwrap(), true).is_ok()
            }));
        }
        assert_eq!(
            workers
                .into_iter()
                .filter_map(|worker| worker.join().ok())
                .filter(|ok| *ok)
                .count(),
            1
        );
    }

    #[test]
    fn worktree_id_is_stable_and_uses_git_metadata() {
        assert_eq!(
            worktree_id(Path::new("C:/repo/.git/worktrees/a")),
            worktree_id(Path::new("c:/REPO/.git/worktrees/a"))
        );
        assert_ne!(
            worktree_id(Path::new("C:/repo/.git/worktrees/a")),
            worktree_id(Path::new("C:/repo/.git/worktrees/b"))
        );
    }

    #[test]
    fn primary_worktree_is_rejected_by_default() {
        let temporary = tempdir().unwrap();
        git(temporary.path(), &["init", "-b", "main"]);
        git(
            temporary.path(),
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
        let state = tempdir().unwrap();
        let runtime = Runtime::with_paths(
            temporary.path().to_path_buf(),
            state.path().join("agent-runtime"),
        );
        assert!(
            runtime
                .prepare(PrepareOptions {
                    allow_primary: false
                })
                .is_err()
        );
        assert!(
            runtime
                .prepare(PrepareOptions {
                    allow_primary: true
                })
                .unwrap()
                .is_primary
        );
    }

    #[test]
    fn contract_event_rejects_uncommitted_api_changes() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
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
        let linked = temporary.path().join("linked");
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "candidate",
                linked.to_str().unwrap(),
            ],
        );
        let runtime = Runtime::with_paths(linked.clone(), temporary.path().join("state"));
        runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        fs::write(linked.join("PendingApi.kt"), "public fun pendingApi() = 1").unwrap();
        let error = runtime
            .create_contract_event(ContractEventOptions {
                provider: "provider",
                base: "1.0.0",
                baseline: None,
                module: None,
                artifact: None,
                consumer_targets: None,
                json: true,
            })
            .unwrap_err();
        assert!(error.to_string().contains("未提交改动"));
    }

    #[test]
    fn contract_snapshot_ignores_test_sources_line_numbers_and_method_bodies() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
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
        let runtime = Runtime::with_paths(repository.clone(), temporary.path().join("state"));
        runtime
            .prepare(PrepareOptions {
                allow_primary: true,
            })
            .unwrap();
        let production = repository.join("module/src/main/kotlin");
        let tests = repository.join("module/src/test/kotlin");
        fs::create_dir_all(&production).unwrap();
        fs::create_dir_all(&tests).unwrap();
        let source = production.join("Api.kt");
        fs::write(&source, "class Api {\n fun start() { println(1) }\n}\n").unwrap();
        fs::write(tests.join("ApiTest.kt"), "fun testOnly() {}\n").unwrap();
        let before = runtime.contract_report(Some("module")).unwrap();
        assert_eq!(before["symbol_count"], 2);
        fs::write(&source, "\n\nclass Api {\n fun start() { println(2) }\n}\n").unwrap();
        let after = runtime.contract_report(Some("module")).unwrap();
        assert_eq!(before["contract_sha256"], after["contract_sha256"]);
        fs::write(&source, "class Api {\n fun observe() { println(2) }\n}\n").unwrap();
        let renamed = runtime.contract_report(Some("module")).unwrap();
        assert_ne!(before["contract_sha256"], renamed["contract_sha256"]);
    }

    #[test]
    fn contract_baseline_rejects_legacy_snapshot_format() {
        let legacy = serde_json::json!({"symbols": ["Api.kt:1:fun start() {}"]});
        assert!(symbols_from_report(&legacy).is_err());
        let current = serde_json::json!({
            "snapshot_format": "kotlin-production-declarations-v2",
            "symbols": ["src/main/kotlin/Api.kt:fun start()"],
        });
        assert_eq!(symbols_from_report(&current).unwrap().len(), 1);
    }

    #[test]
    fn fake_device_registry_assigns_one_device_to_only_one_worktree() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
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
        let first_path = temporary.path().join("first");
        let second_path = temporary.path().join("second");
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "agent-one",
                first_path.to_str().unwrap(),
            ],
        );
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "agent-two",
                second_path.to_str().unwrap(),
            ],
        );
        let state = temporary.path().join("state");
        let first_runtime = Runtime::with_paths(first_path, state.clone());
        let second_runtime = Runtime::with_paths(second_path, state.clone());
        let first = first_runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let second = second_runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let devices = vec!["emulator-5554".to_string()];
        let lease = acquire_device_lease_from_devices(
            &first_runtime,
            &first,
            "emulator-5554",
            60,
            &devices,
        )
        .unwrap();
        assert!(
            acquire_device_lease_from_devices(
                &second_runtime,
                &second,
                "emulator-5554",
                60,
                &devices
            )
            .is_err()
        );
        remove_device_lease(&state, &lease).unwrap();
    }

    #[test]
    fn concurrent_device_acquisition_for_one_worktree_creates_one_lease() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
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
        let worktree = temporary.path().join("agent-worktree");
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "agent-one",
                worktree.to_str().unwrap(),
            ],
        );
        let state = temporary.path().join("state");
        let runtime = Runtime::with_paths(worktree, state.clone());
        let metadata = runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let devices = vec!["fake-device-a".to_string(), "fake-device-b".to_string()];
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let workers = devices
            .iter()
            .map(|serial| {
                let runtime = runtime.clone();
                let metadata = metadata.clone();
                let devices = devices.clone();
                let barrier = barrier.clone();
                let serial = serial.clone();
                thread::spawn(move || {
                    barrier.wait();
                    acquire_device_lease_from_devices(&runtime, &metadata, &serial, 60, &devices)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        let leases = results
            .into_iter()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert_eq!(leases.len(), 1, "一个 Worktree 只能持有一个设备租约");
        let local_lease: DeviceLease =
            read_json(&PathBuf::from(&metadata.paths.root).join("device-lease.json")).unwrap();
        assert_eq!(local_lease.token, leases[0].token);
        assert!(device_lease_path(&state, &leases[0].serial).exists());
        let other_serial = devices
            .iter()
            .find(|serial| **serial != leases[0].serial)
            .unwrap();
        assert!(!device_lease_path(&state, other_serial).exists());
        remove_device_lease(&state, &leases[0]).unwrap();
    }

    #[test]
    fn device_serial_storage_uses_collision_resistant_names() {
        let state = Path::new("state-root");
        assert_ne!(
            device_lease_path(state, "device/one"),
            device_lease_path(state, "device_one")
        );
    }

    #[test]
    fn adb_arguments_cannot_override_the_lease_device_or_stop_the_shared_server() {
        assert!(validate_adb_args(&["install".to_string(), "app.apk".to_string()]).is_ok());
        assert!(validate_adb_args(&["shell".to_string(), "getprop".to_string()]).is_ok());
        assert!(validate_adb_args(&["logcat".to_string(), "-d".to_string()]).is_ok());
        assert!(validate_adb_args(&["logcat".to_string()]).is_ok());
        assert!(
            validate_adb_args(&[
                "-s".to_string(),
                "other-device".to_string(),
                "install".to_string()
            ])
            .is_err()
        );
        assert!(validate_adb_args(&["kill-server".to_string()]).is_err());
    }

    #[test]
    fn android_test_uses_a_fresh_gradle_process_for_the_leased_serial() {
        assert_eq!(
            android_test_gradle_args(&["connectedAndroidTest".to_string()]),
            vec![
                "connectedAndroidTest".to_string(),
                "--no-daemon".to_string()
            ]
        );
        assert_eq!(
            android_test_gradle_args(&["--no-daemon".to_string(), "check".to_string()]),
            vec!["--no-daemon".to_string(), "check".to_string()]
        );
    }

    #[cfg(windows)]
    #[test]
    fn failed_adb_command_releases_child_locks_and_device_lease() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
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
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );
        let state = temp.path().join("state");
        let runtime = Runtime::with_paths(linked, state.clone());
        let metadata = runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let lease = acquire_device_lease_from_devices(
            &runtime,
            &metadata,
            "fake-device-failure",
            60,
            &["fake-device-failure".to_string()],
        )
        .unwrap();

        let fake_adb = temp.path().join("fake-adb.cmd");
        fs::write(
            &fake_adb,
            "@echo off\r\necho simulated-stdout\r\necho simulated-stderr 1>&2\r\nexit /b 23\r\n",
        )
        .unwrap();
        let mut command = Command::new("cmd.exe");
        command.args(["/C", "call"]).arg(&fake_adb);
        let result =
            runtime.run_adb_with_command(command, &["install".to_string(), "fake.apk".to_string()]);
        assert!(result.is_err(), "ADB 子进程失败必须向调用方报告");

        assert!(!device_lease_path(&state, &lease.serial).exists());
        assert!(
            !PathBuf::from(&metadata.paths.root)
                .join("device-lease.json")
                .exists()
        );
        assert!(
            !PathBuf::from(&metadata.paths.root)
                .join("device-lease.token")
                .exists()
        );
        assert!(
            !PathBuf::from(&metadata.paths.root)
                .join("locks/device-operation.json")
                .exists()
        );
        assert!(
            !PathBuf::from(&metadata.paths.worktree_root)
                .join("locks/build.json")
                .exists()
        );
        let logs = fs::read_dir(&metadata.paths.logs)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| fs::read_to_string(entry.path()).unwrap_or_default())
            .collect::<String>();
        assert!(logs.contains("simulated-stdout"));
        assert!(logs.contains("simulated-stderr"));
    }

    #[test]
    fn linked_worktree_gets_isolated_runtime_and_stable_metadata() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
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
        let linked = temporary.path().join("linked");
        git(
            &repository,
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );
        let state = temporary.path().join("state");
        let runtime = Runtime::with_paths(linked.clone(), state.clone());
        let first = runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let again = runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        assert_eq!(first.runtime_id, again.runtime_id);
        assert!(Path::new(&first.paths.gradle_user_home).starts_with(&state));
        assert!(!first.is_primary);
    }

    #[test]
    fn prepare_does_not_erase_existing_port_records() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
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
        let linked = temporary.path().join("linked");
        git(
            &repository,
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );
        let state = temporary.path().join("state");
        let runtime = Runtime::with_paths(linked, state.clone());
        let metadata = runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let record = PortLease {
            schema_version: SCHEMA_VERSION,
            runtime_id: metadata.runtime_id.clone(),
            owner: "tester".into(),
            name: "web".into(),
            port: 18991,
            created_at: Utc::now().to_rfc3339(),
            expires_at: (Utc::now() + Duration::days(7)).to_rfc3339(),
            status: "active".into(),
        };
        write_json_atomic(
            &state.join(APP_DIR).join("ports.json"),
            &vec![record.clone()],
        )
        .unwrap();
        runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let records: Vec<PortLease> = read_json(&state.join(APP_DIR).join("ports.json")).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].port, record.port);
    }

    #[test]
    fn two_agents_share_worktree_cache_and_build_lock_but_keep_agent_state_separate() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
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
        let linked = temporary.path().join("linked");
        git(
            &repository,
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );
        let state = temporary.path().join("state");
        let first_runtime = Runtime::with_paths(linked.clone(), state.clone())
            .with_agent_id("agent-one")
            .unwrap();
        let second_runtime = Runtime::with_paths(linked, state)
            .with_agent_id("agent-two")
            .unwrap();
        let first = first_runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let second = second_runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        assert_eq!(first.paths.worktree_root, second.paths.worktree_root);
        assert_ne!(first.paths.root, second.paths.root);
        assert_ne!(first.paths.logs, second.paths.logs);
        assert_eq!(first.paths.gradle_user_home, second.paths.gradle_user_home);
        let token = acquire_lock_internal(&first_runtime, 0, 60, Some("agent-one")).unwrap();
        assert!(acquire_lock_internal(&second_runtime, 0, 60, Some("agent-two")).is_err());
        assert!(
            second_runtime
                .release_build_lock(Some(&token), true)
                .is_err()
        );
        first_runtime
            .release_build_lock(Some(&token), true)
            .unwrap();
    }

    #[test]
    fn lifecycle_agent_identity_is_stable_and_distinct_from_codex_identity() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
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
        let linked = temporary.path().join("linked");
        git(
            &repository,
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );
        let lifecycle = Runtime::lifecycle_agent_id(&linked).unwrap();
        let runtime = Runtime::with_paths(linked.clone(), temporary.path().join("state"))
            .with_agent_id(&lifecycle)
            .unwrap();
        let metadata = runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        assert!(metadata.agent_id.starts_with("worktrunk-"));
        assert_eq!(lifecycle, Runtime::lifecycle_agent_id(&linked).unwrap());
        assert_ne!(metadata.agent_id, "codex-agent");
    }

    #[test]
    fn removed_worktree_can_be_resolved_by_repository_and_branch() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
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
        let linked = temporary.path().join("linked");
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "feature-removed",
                linked.to_str().unwrap(),
            ],
        );
        let state = temporary.path().join("state");
        let runtime = Runtime::with_paths(repository.clone(), state.clone());
        let agent = Runtime::with_paths(linked.clone(), state.clone())
            .with_agent_id("codex-agent")
            .unwrap();
        let metadata = agent
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let lifecycle = Runtime::lifecycle_agent_id(&linked).unwrap();
        Runtime::with_paths(linked.clone(), state.clone())
            .with_agent_id(&lifecycle)
            .unwrap()
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();

        let (id, path) = runtime.find_worktree_by_branch("feature-removed").unwrap();
        assert_eq!(id, metadata.worktree_id);
        assert_eq!(path_identity(&path), path_identity(&linked));
    }

    #[test]
    fn build_lock_is_exclusive_and_requires_owner_token_to_release() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
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
        let linked = temporary.path().join("linked");
        git(
            &repository,
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );
        let runtime = Runtime::with_paths(linked, temporary.path().join("state"));
        runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let token = acquire_lock_internal(&runtime, 0, 60, Some("test-agent")).unwrap();
        assert!(acquire_lock_internal(&runtime, 0, 60, Some("second-agent")).is_err());
        assert!(runtime.unlock_build(Some("wrong-token")).is_err());
        runtime.release_build_lock(Some(&token), true).unwrap();
    }

    #[test]
    fn cache_install_copies_into_private_home_and_respects_build_lock() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
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
        let linked = temporary.path().join("linked");
        git(
            &repository,
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );
        let wrapper = linked.join("gradle/wrapper/gradle-wrapper.properties");
        fs::create_dir_all(wrapper.parent().unwrap()).unwrap();
        fs::write(
            &wrapper,
            "distributionUrl=https://example.invalid/gradle.zip\n",
        )
        .unwrap();
        let digest: String = Sha256::digest(fs::read(&wrapper).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let snapshot = temporary.path().join("snapshot");
        fs::create_dir_all(snapshot.join("gradle-dependencies/modules-2/files-2.1")).unwrap();
        fs::create_dir_all(snapshot.join("gradle-distribution/gradle-8.10.2")).unwrap();
        fs::create_dir_all(snapshot.join("maven-test-runtime/org/robolectric/android-all"))
            .unwrap();
        fs::write(
            snapshot.join("gradle-dependencies/modules-2/files-2.1/example.jar"),
            b"dependency",
        )
        .unwrap();
        fs::write(
            snapshot.join("gradle-distribution/gradle-8.10.2/gradle.zip"),
            b"zip",
        )
        .unwrap();
        fs::write(
            snapshot.join("maven-test-runtime/org/robolectric/android-all/runtime.jar"),
            b"robolectric",
        )
        .unwrap();
        write_json_atomic(
            &snapshot.join("manifest.json"),
            &serde_json::json!({"schema_version":1,"coverage":"partial","wrapper_properties_sha256":digest}),
        )
        .unwrap();
        let runtime = Runtime::with_paths(linked, temporary.path().join("state"));
        let metadata = runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let token = acquire_lock_internal(&runtime, 0, 60, None).unwrap();
        assert!(runtime.install_cache(snapshot.to_str().unwrap()).is_err());
        runtime.release_build_lock(Some(&token), true).unwrap();
        runtime.install_cache(snapshot.to_str().unwrap()).unwrap();
        let home = PathBuf::from(&metadata.paths.gradle_user_home);
        assert_eq!(
            fs::read(home.join("caches/modules-2/files-2.1/example.jar")).unwrap(),
            b"dependency"
        );
        assert!(
            home.join("wrapper/dists/gradle-8.10.2/gradle.zip")
                .is_file()
        );
        assert_eq!(
            fs::read(
                PathBuf::from(&metadata.paths.maven_local)
                    .join("org/robolectric/android-all/runtime.jar")
            )
            .unwrap(),
            b"robolectric"
        );
        assert!(runtime.install_cache(snapshot.to_str().unwrap()).is_err());
    }

    #[test]
    fn doctor_does_not_recover_an_unexpired_lock_after_owner_exit() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
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
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );
        let state = temp.path().join("state");
        let runtime = Runtime::with_paths(linked, state);
        let metadata = runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let path = PathBuf::from(&metadata.paths.worktree_root).join("locks/build.json");
        let now = Utc::now();
        let lock = BuildLock {
            schema_version: SCHEMA_VERSION,
            runtime_id: metadata.runtime_id.clone(),
            owner: metadata.agent_id.clone(),
            pid: i32::MAX as u32,
            process_start_time: base64(b"not-a-live-process"),
            token: "expired-owner-token".to_string(),
            created_at: now.to_rfc3339(),
            expires_at: (now + Duration::minutes(5)).to_rfc3339(),
            heartbeat_at: now.to_rfc3339(),
            gradle_pid: None,
            gradle_process_start_time: None,
        };
        write_json_atomic(&path, &lock).unwrap();

        assert!(runtime.recover_stale_worktree_lock(&metadata).is_err());
        assert_eq!(read_json::<BuildLock>(&path).unwrap().token, lock.token);
    }

    #[test]
    fn doctor_recovers_unexpired_run_lock_after_creator_and_child_exit() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
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
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );
        let state = temp.path().join("state");
        let runtime = Runtime::with_paths(linked, state);
        let metadata = runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let path = PathBuf::from(&metadata.paths.worktree_root).join("locks/build.json");
        let now = Utc::now();
        let lock = BuildLock {
            schema_version: SCHEMA_VERSION,
            runtime_id: metadata.runtime_id.clone(),
            owner: metadata.agent_id.clone(),
            pid: i32::MAX as u32,
            process_start_time: base64(b"dead-creator"),
            token: "run-lock-token".to_string(),
            created_at: now.to_rfc3339(),
            expires_at: (now + Duration::minutes(5)).to_rfc3339(),
            heartbeat_at: now.to_rfc3339(),
            gradle_pid: Some(i32::MAX as u32 - 1),
            gradle_process_start_time: Some(base64(b"dead-child")),
        };
        write_json_atomic(&path, &lock).unwrap();

        runtime.recover_stale_worktree_lock(&metadata).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn force_cleanup_keeps_a_live_device_operation_lock() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
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
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );
        let state = temp.path().join("state");
        let runtime = Runtime::with_paths(linked, state);
        let metadata = runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let operation_path =
            PathBuf::from(&metadata.paths.root).join("locks/device-operation.json");
        let now = Utc::now();
        let mut holder = Command::new(if cfg!(windows) {
            "powershell.exe"
        } else {
            "sleep"
        });
        if cfg!(windows) {
            holder.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep 30",
            ]);
        } else {
            holder.arg("30");
        }
        let mut holder = holder.spawn().unwrap();
        let holder_start = live_process_identity(holder.id());
        assert!(!holder_start.is_empty());
        let operation = TestLease {
            schema_version: SCHEMA_VERSION,
            runtime_id: metadata.runtime_id.clone(),
            agent_id: metadata.agent_id.clone(),
            serial: "fake-device".to_string(),
            pid: holder.id(),
            process_start_time: holder_start.clone(),
            token: "live-operation-token".to_string(),
            acquired_at: now.to_rfc3339(),
            expires_at: (now + Duration::hours(1)).to_rfc3339(),
            heartbeat_at: now.to_rfc3339(),
        };
        write_json_atomic(&operation_path, &operation).unwrap();
        assert!(holder.try_wait().unwrap().is_none());

        let result = runtime.cleanup(false, false, true, false, None, None);
        assert!(result.is_err(), "cleanup result: {result:?}");
        assert_eq!(
            read_json::<TestLease>(&operation_path).unwrap().token,
            operation.token
        );
        let _ = holder.kill();
    }

    #[test]
    fn cleanup_boundary_and_gradle_argument_checks_are_strict() {
        let temporary = tempdir().unwrap();
        let state = temporary.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let runtime = Runtime::with_paths(temporary.path().to_path_buf(), state.clone());
        assert!(
            runtime
                .assert_inside_state_root(&temporary.path().join("outside"))
                .is_err()
        );
        assert!(
            runtime
                .assert_inside_state_root(&state.join("worktrees/missing/gradle-user-home"))
                .is_ok()
        );
        assert!(
            runtime
                .assert_inside_state_root(&state.join("../outside"))
                .is_err()
        );
        assert!(validate_gradle_args(&["clean".into()], false).is_err());
        assert!(validate_gradle_args(&["--stop".into()], true).is_err());
        assert!(validate_gradle_args(&["assemble".into()], false).is_ok());
    }

    #[test]
    fn corrupt_state_lock_is_never_recovered_automatically() {
        let temporary = tempdir().unwrap();
        let lock = temporary.path().join("state.lock");
        fs::write(&lock, "not-a-valid-state-lock").unwrap();
        let old = FileTimes::new().set_modified(SystemTime::now() - StdDuration::from_secs(180));
        fs::File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_times(old)
            .unwrap();

        assert!(recover_state_lock_if_stale(&lock).is_err());
        assert_eq!(fs::read_to_string(&lock).unwrap(), "not-a-valid-state-lock");
    }

    #[test]
    fn fake_adb_filters_offline_and_unauthorized_devices() {
        let temp = tempdir().unwrap();
        let fake = temp.path().join("adb.cmd");
        fs::write(&fake, "@echo off\necho List of devices attached\necho serial-ok\tdevice\necho serial-offline\toffline\necho serial-unauthorized\tunauthorized\n").unwrap();
        let devices = adb_devices_with(&fake).unwrap();
        assert_eq!(devices, vec!["serial-ok"]);
        assert_eq!(
            parse_adb_device_statuses(
                "List of devices attached\nserial-ok\tdevice\nserial-offline\toffline\nserial-unauthorized\tunauthorized\n"
            ),
            vec![
                serde_json::json!({"serial":"serial-ok", "status":"device"}),
                serde_json::json!({"serial":"serial-offline", "status":"offline"}),
                serde_json::json!({"serial":"serial-unauthorized", "status":"unauthorized"}),
            ]
        );
    }

    #[test]
    fn adb_serial_with_spaces_is_preserved_in_queries_and_device_selection() {
        let text = "List of devices attached\nadb-wireless-name (2)._adb-tls-connect._tcp\tdevice\nusb-device\toffline\nrestricted\tno permissions (check udev rules)\nlegacy-device device\n\n";
        assert_eq!(
            parse_adb_devices(text),
            vec![
                "adb-wireless-name (2)._adb-tls-connect._tcp",
                "legacy-device",
            ]
        );
        let statuses = parse_adb_device_statuses(text);
        assert_eq!(statuses.len(), 4);
        assert_eq!(
            statuses[0]["serial"],
            "adb-wireless-name (2)._adb-tls-connect._tcp"
        );
        assert_eq!(statuses[0]["status"], "device");
        assert_eq!(statuses[2]["status"], "no permissions");
    }

    #[test]
    fn fake_device_registry_assigns_one_device_to_only_one_worktree_with_serial() {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
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
        let first_path = temporary.path().join("first");
        let second_path = temporary.path().join("second");
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "agent-one",
                first_path.to_str().unwrap(),
            ],
        );
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "agent-two",
                second_path.to_str().unwrap(),
            ],
        );
        let state = temporary.path().join("state");
        let first_runtime = Runtime::with_paths(first_path, state.clone());
        let second_runtime = Runtime::with_paths(second_path, state.clone());
        let first = first_runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let second = second_runtime
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let devices = vec!["fake-device-01".to_string()];
        let lease = acquire_device_lease_from_devices(
            &first_runtime,
            &first,
            "fake-device-01",
            60,
            &devices,
        )
        .unwrap();
        assert!(
            acquire_device_lease_from_devices(
                &second_runtime,
                &second,
                "fake-device-01",
                60,
                &devices
            )
            .is_err()
        );
        remove_device_lease(&state, &lease).unwrap();
    }

    #[test]
    fn metadata_creation_isolated_by_worktree_and_agent_identity_is_stable() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
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
        let linked_a = temp.path().join("linked-a");
        let linked_b = temp.path().join("linked-b");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feature-a",
                linked_a.to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feature-b",
                linked_b.to_str().unwrap(),
            ],
        );
        let state = temp.path().join("state");
        let first = Runtime::with_paths(linked_a, state.clone())
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        let second = Runtime::with_paths(linked_b, state.clone())
            .prepare(PrepareOptions {
                allow_primary: false,
            })
            .unwrap();
        assert_ne!(first.runtime_id, second.runtime_id);
        assert_ne!(first.paths.gradle_user_home, second.paths.gradle_user_home);
    }

    #[test]
    fn base_and_heap_validation_rejects_out_of_range_values() {
        assert!(valid_heap_size("2g"));
        assert!(valid_heap_size("1536m"));
        assert!(!valid_heap_size("0g"));
        assert!(!valid_heap_size("1gb"));
        assert!(
            parse_adb_devices("List of devices attached\nphone-a\tdevice\nphone-b\toffline\n")
                .contains(&"phone-a".to_string())
        );
        assert!(
            !parse_adb_devices("List of devices attached\nphone-b\toffline\n")
                .contains(&"phone-b".to_string())
        );
    }

    #[test]
    fn powershell_quoting_and_base64_are_deterministic() {
        assert_eq!(ps_quote("a'b"), "'a''b'");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }

    #[test]
    fn candidate_artifact_rejects_partial_evidence_and_different_version() {
        let good = ArtifactEvidence {
            url: Some("https://example.invalid/provider.aar".into()),
            sha256: Some("a".repeat(64)),
            coordinate: Some("example:provider:0.4.0-dev.fcaeddb00a44".into()),
        };
        assert!(validate_artifact_version(&good, "0.4.0-dev.fcaeddb00a44").is_ok());
        assert!(validate_artifact_version(&good, "0.4.0-dev.fcaeddb00a442").is_err());
        let mut partial = good.clone();
        partial.sha256 = None;
        assert!(validate_artifact_version(&partial, "0.4.0-dev.fcaeddb00a44").is_err());
        for version in [
            "1.+",
            "latest.release",
            "1.0-SNAPSHOT",
            "1.0-snapshot",
            "a:b",
            "a/b",
            "a b",
            "",
        ] {
            assert!(!fixed_version(version));
        }
        let empty = ArtifactEvidence {
            url: None,
            sha256: None,
            coordinate: None,
        };
        assert!(validate_artifact_version(&empty, "1.0.0").is_ok());
    }

    #[test]
    fn importing_mismatched_candidate_does_not_create_event() {
        let temp = tempdir().unwrap();
        let runtime = Runtime::with_paths(temp.path().into(), temp.path().join("state"));
        let path = temp.path().join("event.json");
        write_json_atomic(&path, &serde_json::json!({
            "event_id":"mismatch", "event_type":"contract_candidate", "affected_consumers":["consumer"],
            "candidate_version":"0.4.0-dev.fcaeddb00a44",
            "artifact":{"coordinate":"example:provider:0.4.0-dev.fcaeddb00a442", "url":"https://example.invalid/provider.aar", "sha256":"a".repeat(64)}
        })).unwrap();
        assert!(runtime.import_event(path.to_str().unwrap(), true).is_err());
        assert!(!runtime.state_root.join("events/mismatch.json").exists());
    }
}
