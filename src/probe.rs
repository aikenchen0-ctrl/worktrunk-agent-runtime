//! 诊断查询使用文件接收输出，避免后台服务继承管道后阻塞调用方。
use super::*;

pub(super) fn adb_devices() -> Result<Output> {
    let seconds = env::var("AGENT_ADB_QUERY_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "10".into())
        .parse::<u64>()
        .ok()
        .filter(|seconds| (1..=300).contains(seconds))
        .ok_or_else(|| anyhow::anyhow!("AGENT_ADB_QUERY_TIMEOUT_SECONDS 必须是 1 到 300 的秒数"))?;
    bounded_output(
        Command::new("adb").arg("devices"),
        StdDuration::from_secs(seconds),
    )
    .context("查询 ADB 设备失败")
}

fn bounded_output(command: &mut Command, timeout: StdDuration) -> Result<Output> {
    let stdout = NamedTempFile::new().context("创建查询标准输出文件失败")?;
    let stderr = NamedTempFile::new().context("创建查询标准错误文件失败")?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.as_file().try_clone()?))
        .stderr(Stdio::from(stderr.as_file().try_clone()?))
        .spawn()
        .context("启动诊断查询进程失败")?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(Output {
                    status,
                    stdout: fs::read(stdout.path())?,
                    stderr: fs::read(stderr.path())?,
                });
            }
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(StdDuration::from_millis(25));
            }
            Ok(None) => {
                // 只终止本次查询客户端，不终止它可能启动的共享 ADB Server。
                child.kill().context("终止超时诊断查询客户端失败")?;
                child.wait().context("回收超时诊断查询客户端失败")?;
                bail!(
                    "诊断查询超过 {} 毫秒；已终止本次客户端，未停止共享服务",
                    timeout.as_millis()
                );
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("检查诊断查询进程失败");
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_query_preserves_successful_streams() {
        let mut command = Command::new("cmd.exe");
        command.args([
            "/D",
            "/C",
            "echo probe-out & echo probe-err 1>&2 & exit /b 0",
        ]);
        let output = bounded_output(&mut command, StdDuration::from_secs(5)).unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("probe-out"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("probe-err"));
    }

    #[test]
    fn diagnostic_query_preserves_nonzero_exit_code() {
        let output = bounded_output(
            Command::new("cmd.exe").args(["/D", "/C", "exit /b 23"]),
            StdDuration::from_secs(5),
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(23));
    }

    #[test]
    fn diagnostic_query_has_a_bounded_wait() {
        let started = Instant::now();
        let error = bounded_output(
            Command::new("powershell.exe").args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ]),
            StdDuration::from_millis(200),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("超过 200 毫秒"));
        assert!(started.elapsed() < StdDuration::from_secs(10));
    }
}
