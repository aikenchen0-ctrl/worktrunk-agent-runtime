# 发布与安装指南

## 1. 发布定位

本仓库交付 Windows x64 本机 Worktree 控制器预览版，不是已经上线的全组织自动发布平台。

本地构建隔离、设备互斥、候选事件与验证收据属于控制器职责；迁移调用、撰写接口说明、在授权后操作 PR 属于三个 Skill 的工作流。远端禁止覆盖、跨机器可信身份、持续通知投递、CI 证明与发布晋升仍需独立基础设施，不得用提示词代替。

历史 Android 报告保留其原始测试范围；本次发布包测试不代表重新执行了十几个 Desktop Agent、真机和远端服务的端到端验证。

## 2. 本地发布检查

在控制器仓库根目录执行，不在业务 AAR 仓库执行：

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

每一步退出码必须为 0。CI 使用 Rust 1.96.0，并单独检查声明的最低版本 1.85.0。

```powershell
.\scripts\package-release.ps1
if ($LASTEXITCODE -ne 0) { throw '打包失败' }
$package = (Resolve-Path '.\dist\agent-runtime-0.1.0-windows-x86_64').Path
& powershell.exe -NoProfile -NonInteractive -File .\scripts\test-release.ps1 -PackageRoot $package
if ($LASTEXITCODE -ne 0) { throw '安装回归失败' }
cargo package --list --locked --allow-dirty
```

打包脚本不覆盖已存在的输出。重试使用新的 `-OutputRoot`；不要把上次失败时留下的目录当成成功制品。`-SkipBuild` 仅用于已由本轮构建得到的同版本二进制，不能替代重新构建。ZIP 内含文件摘要清单，旁边另有 ZIP 的 SHA-256 文件。

自动回归覆盖新安装、状态保留、重复安装拒绝、篡改包拒绝、越界路径拒绝、安装互斥和 Hook 退出码，不包含签名验证、自动升级或远端发布。

## 3. 安装

先从可信渠道核对 ZIP 的 SHA-256，再手工解压。不要直接执行未知下载地址中的脚本。

```powershell
$package = 'F:\tools\downloads\agent-runtime-0.1.0-windows-x86_64'
& powershell.exe -NoProfile -NonInteractive -File "$package\scripts\install.ps1" -PackageRoot $package
if ($LASTEXITCODE -ne 0) { throw '安装失败' }
$agentctl = Join-Path $env:LOCALAPPDATA 'agent-runtime\bin\agentctl.exe'
& $agentctl --version
```

安装器只新增程序文件，不修改全局 PATH、Git 配置、Android 项目、运行状态或用户 Skill。默认目录若已有旧安装，会拒绝覆盖。需要并行验证新版本时，用 `-InstallRoot 'F:\tools\agent-runtime-0.1.0'`，再将调用入口和 Hook 指向该目录；所有 Agent 仍必须使用同一个 `AGENT_RUNTIME_ROOT`。

新目录验证通过后再由维护者切换程序路径。停止相关构建、保留旧二进制用于回退；不要递归删除默认安装根，因为其下可能同时保存租约、事件、日志与缓存。安装被强制中断时，先确认没有安装进程，再人工检查 `.install.lock` 与 `.install-*`；当前不提供自动恢复或覆盖升级。

## 4. 接入 Worktrunk 与 Agent

- Git、Windows PowerShell 5.1、业务所需 JDK/SDK/ADB 由机器统一配置。控制器不会安装 Android 工具链。
- 安装目录中的 `examples/wt.toml` 是 Windows Hook 模板，复制并审查后放入业务仓库的 `.config/wt.toml`。已有配置必须合并，不覆盖其他 Hook。
- `worktrunk-hook.ps1` 要求 Worktrunk 支持 JSON 标准输入上下文，已核对 0.79.0 的该能力；升级 Worktrunk 后应重测。`pre-merge` 默认运行 `check`，项目验收任务不同则显式调整适配脚本。
- Hook 默认只清理已登记资源，保留缓存，不使用 `--force`。同一 Worktree 若同时有 Codex 与生命周期 runtime，先由每个 Agent 执行自己的 `cleanup`，再移除 Worktree；拒绝清理是保护，不应改成强制放行。
- Codex 自行创建的 Worktree 不一定触发 Worktrunk Hook，Agent 仍应在首次进入时执行 `agentctl prepare`，每次构建走 `run-gradle`。
- 每个 Agent 在自己的 Worktree 中工作；不要给每个 Agent 设置不同的状态根目录。不要让全部 Agent 在同一个源码目录中并行写入。
- 三个 Skill 位于安装包 `skills` 中；按 `docs/codex-skills-guide.md` 安装，已有同名 Skill 先比较再更新。安装器不会覆盖用户 Skill。

## 5. 发布远端仓库前的人工门槛

1. 确认仓库归属、名称、公开或私有，以及源代码和随包材料的发布权限。
2. 复核既有 `MIT OR Apache-2.0` 授权选择与依赖许可证。
3. 检查暂存清单，禁止 `.runtime`、`.tools`、缓存、设备状态、构建产物和凭证进入 Git。
4. 配置分支保护和 CI 必需检查；先通过一次远端 CI，再创建预发布标签。
5. 启用私密漏洞报告。二进制尚未签名时明确标记；需要组织签名时先签名，再重新打包与核对摘要。
6. 人工审核变更记录、安装回归和已知限制，再发布 ZIP 与 SHA-256。

本地脚本只准备制品，CI 上传的也是待审核 artifact；不会创建公开仓库、推送标签、自动发布 GitHub Release 或发布到 crates.io。
