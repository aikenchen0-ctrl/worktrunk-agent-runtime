# Codex Desktop 多 Worktree 使用流程

> 文档中的用户目录已匿名化；示例绝对路径须按实际机器替换。

适用日期：2026-09-27。当前接入方式是各 Codex 任务调用本机 `agentctl`，不是让它们在同一个目录中并行修改。外挂不需要启动常驻服务；跨任务自动唤醒、跨仓升级 PR 自动化尚未接入。

## 一次性安装

首次安装使用经过摘要校验的完整发布包。已有旧安装时不要直接覆盖，按 [发布与安装指南](release-guide.md) 安装到新目录验证后再切换。

```powershell
$runtimePackage = 'F:\tools\downloads\agent-runtime-0.1.0-windows-x86_64'
$runtimeInstall = Join-Path $env:LOCALAPPDATA 'agent-runtime'
& powershell.exe -NoProfile -NonInteractive -File "$runtimePackage\scripts\install.ps1" -PackageRoot $runtimePackage -InstallRoot $runtimeInstall
if ($LASTEXITCODE -ne 0) { throw '安装失败' }
& "$runtimeInstall\bin\agentctl.exe" --help
```

自行构建时，在外挂源码目录运行 `scripts/package-release.ps1` 并完成发布回归。二进制和配套脚本必须按同一批次安装；`--version` 的 `0.1.0` 不能区分各次修复，应同时记录包 SHA-256 与提交号。

无需修改全局 PATH。以下示例使用完整安装路径。所有 Agent 和 Worktrunk hook 必须统一 `AGENT_RUNTIME_ROOT=F:\ar`，不能各用各的状态根目录。不要设置全局 `AGENT_ID`。

修复后的 `agentctl env --format powershell` 输出可以直接交给 `Invoke-Expression` 执行；仍建议每个 Desktop 命令块显式设置 `AGENT_ID`，不要将 Agent ID 写成全局用户变量。

## 建立任务与目录的对应关系

| Codex 任务 | 实际工作目录 | 稳定 Agent ID | 职责 |
| --- | --- | --- | --- |
| Android 模块开发 | 独立 Android Worktree 根目录 | android-dev-01 | 该模块业务及接口 |
| Compose 模块开发 | 独立 Compose Worktree 根目录 | compose-dev-01 | 该模块业务及依赖迁移 |
| 集成验证 | 独立集成 Worktree 根目录 | integration-01 | 固定候选组合并验证 |

任务标题不会自动设置 AGENT_ID。为一个任务选定 ID 后，每次命令沿用；重开任务接续原工作时可沿用原 ID。设备序列号单独分配。

推荐使用 Worktrunk 创建有名称的 Worktree，然后把它的实际目录作为 Codex Desktop 的本地项目打开，在该目录启动本地任务。这里的“本地”指已创建的 Worktree 目录，不是主仓库。避免再次点 Worktree 后生成第二套检出；最终以 `git rev-parse --show-toplevel` 核对。

如果选择让 Codex Desktop 自己创建 Worktree，同样可以运行 `agentctl`，但创建、合并和删除没有经过 Worktrunk 时，不能指望 `.config/wt.toml` 自动执行。此路线必须由 Agent 显式 prepare、验证及 cleanup。[官方 Worktree 说明](https://learn.chatgpt.com/docs/environments/git-worktrees)

在指定样例项目中可由环境准备任务执行以下命令；三个名称是新分支示例，已经存在时不要重复创建：

```powershell
Set-Location 'F:\code\worktree\superAppAndroid-validation'
$env:AGENT_RUNTIME_ROOT = 'F:\ar'
$wt = 'C:\Users\developer\AppData\Local\Microsoft\WinGet\Packages\max-sixty.worktrunk_Microsoft.Winget.Source_8wekyb3d8bbwe\wt.exe'
& $wt switch --create agent-android-01 --base backSelfLanuchOKcnnOK
if ($LASTEXITCODE -ne 0) { throw '创建 Android Worktree 失败' }
& $wt switch --create agent-compose-01 --base backSelfLanuchOKcnnOK
if ($LASTEXITCODE -ne 0) { throw '创建 Compose Worktree 失败' }
& $wt switch --create agent-integration-01 --base backSelfLanuchOKcnnOK
if ($LASTEXITCODE -ne 0) { throw '创建集成 Worktree 失败' }
git worktree list
```

通过 exe 调用 Worktrunk 不依赖 shell 的自动切目录功能；从 `git worktree list` 获取真正生成的路径再打开。不要运行未经核对的裸 `wt`，本机 PATH 可能优先找到 Windows Terminal。

当前样例仍是多模块同仓。不同 Worktree 会包含整仓源码，给 Agent 分配模块范围不等于已经拆成独立仓库；项目依赖也不会自动变成 Maven 依赖。

## 仓库规则

把 [AGENTS.md 模板](../examples/AGENTS.md) 的相关规则合并到业务仓库自己的 AGENTS.md，保留原有业务约束。依赖预下载规则可单独从 [AGENTS-cache.md](../examples/AGENTS-cache.md) 复制。先提交共同规则，再从该提交创建 Worktree；已有 Worktree 需要同步规则。文件名是 `AGENTS.md`。不要将所有 Agent 都指向外挂开发目录。

Codex 会读取适用的 AGENTS.md；当前任务的负责模块和 Agent ID 用首条提示指定，机器路径放启动配置或提示中，避免在跨机器仓库中写死个人安装路径。[官方说明](https://learn.chatgpt.com/docs/agent-configuration/agents-md)

## 可直接交给模块 Agent 的首条提示

替换模块、工作目录和 Agent ID 后发送：

```text
你负责当前 Worktree 中的 univerge-heavy-drag-compose 模块。
预期工作目录：填写刚才 git worktree list 得到的 Compose Worktree 根目录。
稳定 Agent ID：compose-dev-01。
外挂可执行文件：C:\Users\developer\AppData\Local\agent-runtime\bin\agentctl.exe。
配套候选验证脚本：C:\Users\developer\AppData\Local\agent-runtime\scripts\validate-candidate.ps1。
全机统一状态根目录：F:\ar。
JDK：C:\Users\developer\.jdks\ms-17.0.16。
Android SDK：C:\Users\developer\AppData\Local\Android\Sdk。

先读本仓 AGENTS.md，核对 Git 根目录、分支、主 Worktree 状态和未提交修改。
每个新的 shell 调用都显式设置 AGENT_RUNTIME_ROOT、AGENT_ID、JAVA_HOME、ANDROID_SDK_ROOT；
不要依赖上一条命令或桌面终端的环境变量，所有命令在指定 Worktree 根目录执行。
所有 Gradle 和设备操作通过 agentctl；不要直接操作其他 Worktree。
先 prepare、doctor、env --format json，报告实际隔离目录；失败时不回退到共享缓存。
每次开始处理任务、升级前和结束前，读取 event-inbox --consumer univerge-heavy-drag-compose。
只处理与本模块有关的事件；未就绪的上游功能继续使用固定稳定版本或明确标记的草稿桩。
接口或行为变化时，在本仓更新版本化说明和迁移步骤，不编辑共享实时接口文档。
正式确认候选前提交迁移；使用 validate-candidate.ps1，不手写 passed 或构建收据。
发现破坏性变化要记录迁移方案，不能为了通过扫描而禁止一切 API 演化。
报告区分本模块检查、候选验证、组合验证与未完成项，不自动合并或发布。

本次业务任务：在这里填写具体功能与验收条件。
```

读取收件箱是任务工作步骤，不是后台监听服务；任务闲置时不会自行醒来。当前需要你在对应任务发“检查并处理新候选事件”，或让明确授权的协调任务推动它。

## Agent 每个命令块的环境前缀

以下针对本机已实测路径。配置权限时需要允许命令写 `F:\ar` 和当前 Worktree、执行 JDK/ADB，以及在缺依赖时下载。权限不足应报告和申请必要权限，不能切换到另一个私有状态根目录规避互斥。

```powershell
$env:AGENT_RUNTIME_ROOT = 'F:\ar'
$env:AGENT_ID = 'compose-dev-01'
$env:JAVA_HOME = 'C:\Users\developer\.jdks\ms-17.0.16'
$env:ANDROID_SDK_ROOT = 'C:\Users\developer\AppData\Local\Android\Sdk'
$env:ANDROID_HOME = $env:ANDROID_SDK_ROOT
$env:AGENT_MAX_WORKERS = '2'
$env:AGENT_MAX_HEAP = '2g'
$env:AGENT_COMMAND_TIMEOUT_SECONDS = '1800'
$wtDir = 'C:\Users\developer\AppData\Local\Microsoft\WinGet\Packages\max-sixty.worktrunk_Microsoft.Winget.Source_8wekyb3d8bbwe'
$env:PATH = "$wtDir;$env:JAVA_HOME\bin;$env:ANDROID_SDK_ROOT\platform-tools;$env:PATH"
$agentctl = 'C:\Users\developer\AppData\Local\agent-runtime\bin\agentctl.exe'
```

上述资源数值只是起始示例，按机器总资源调整；不能给十几个构建各分配大部分内存。每个命令块在运行任务时都带前缀，或者通过已审查的本机包装脚本设置。`agentctl env` 只查询/输出环境，不会改变父进程；通常无需执行它的 PowerShell 输出，run-gradle 会自行注入 Gradle/Maven 路径。

初始化和日常构建（与环境前缀处于同一个 shell 调用）：

```powershell
& $agentctl prepare
if ($LASTEXITCODE -ne 0) { throw '初始化失败' }
& $agentctl doctor
if ($LASTEXITCODE -ne 0) { throw '诊断未通过，先处理具体失败项' }
& $agentctl env --format json
& $agentctl event-inbox --consumer univerge-heavy-drag-compose --format json
& $agentctl run-gradle -- ':univerge-heavy-drag-compose:testDebugUnitTest'
if ($LASTEXITCODE -ne 0) { throw '模块测试失败' }
```

首次依赖导入按 [Codex Desktop AAR Agent 依赖预下载使用手册](codex-agent-cache-guide.md) 执行。`install-cache` 只适用于完全空的新 Worktree 私有缓存，且必须使用已核验并匹配 Wrapper 的快照；它不等于共享可写缓存，也不是日常构建必需步骤。

## 设备

先查询在线设备再分配，示例序列号仅用于展示格式；使用实际授权且在线的手机。每次操作仍带相同环境前缀。

```powershell
& $agentctl acquire-device --serial '实际设备序列号'
if ($LASTEXITCODE -ne 0) { throw '设备租约获取失败' }
& $agentctl adb -- get-state
& $agentctl run-android-test -- ':app:connectedDebugAndroidTest'
& $agentctl release-device
```

不用手动追加 `adb -s`；外挂从租约注入序列号。设备闲置期间租约会过期，后续先续租或重新获取。只有安装/设备测试需要手机，普通 AAR 编译无需占用设备。

## 候选升级与集成

提供方先发布不可变候选制品，再创建带真实 URL、坐标和摘要的 contract-event。正式跨仓发布配置仍需在业务项目中接入；外挂不会自动部署 Maven 私服或上传 AAR。同机相同状态根目录中的消费者可直接读取事件。`event-import` 只导入事件文件，不提供跨机器仓库身份映射、可信分发或 CI 确认，不能把它当成跨机器协作已完成。

独立仓库的消费者必须通过 `contract-event --consumer-targets <路由JSON>` 登记仓库绝对路径和 Gradle 项目。两个仓库都使用 `:app` 时，使用不同消费者标识，例如 `buyer-app`、`seller-app`。收件箱和验证确认会核对当前仓库归属，不能继续只按模块名匹配。配置格式和旧事件迁移限制见 [本机跨仓路由](consumer-routing.md)。

消费者在自己的 Worktree 迁移并提交后调用：

```powershell
& 'C:\Users\developer\AppData\Local\agent-runtime\scripts\validate-candidate.ps1' `
  -EventFile 'F:\ar\events\实际事件ID.json' `
  -Consumer univerge-heavy-drag-compose `
  -GradleTask ':univerge-heavy-drag-compose:testDebugUnitTest' `
  -CandidateProperty HEAVY_DRAG_ANDROID_VERSION `
  -ConsumerProject ':univerge-heavy-drag-compose' `
  -Configuration debugRuntimeClasspath `
  -AgentCtl $agentctl
```

这些是样例项目的参数，其他仓库须替换。原始业务分支仍是 project 依赖，必须先让该版本属性真正选择 Maven 坐标；光传参数不会完成迁移。实际可用制品应来自持续可访问仓库，不要直接复用已停止的回环测试服务。

集成 Agent 维护包含实际候选事件的 [集成清单](../examples/integration-manifest.json)，在自己干净的 Worktree 中完成组合验证：

```powershell
& $agentctl run-integration --manifest '.config/integration.json' -- '-PHEAVY_DRAG_ANDROID_VERSION=实际候选版本'
if ($LASTEXITCODE -ne 0) { throw '组合验证未通过' }
& $agentctl integration-status --manifest '.config/integration.json' --format json
```

默认需先迁移所有登记消费者并产生有效确认；原样例的 app/accessibility 仍未完成全量验证。联合迁移期间可显式添加 `--allow-pending-consumers` 先运行组合测试，无需伪造确认。此模式不修改消费者状态；即使组合构建通过，仍有待验证消费者时 `ready_for_integration` 为假，`integration-status` 返回失败。组合通过不代表已获发布批准。

供 Agent 自动调用时，`run-integration --format json` 的标准输出仅包含本次构建收据，构建日志和诊断写入标准错误；不要用 `2>&1` 合并后再解析 JSON。启动前的参数或前置检查失败可能没有收据，仍须先检查退出码。PowerShell 中的版本属性整体加引号，例如 `'-PHEAVY_DRAG_ANDROID_VERSION=0.4.0-dev.abcdef123456'`，避免参数被拆分。

## 合并、删除与 Hook

不同仓库分别合并各自分支，通过版本清单组合 AAR，不是把十几个仓库的源码合到一个 Git 分支。相同仓库的多个 Worktree 合并时仍需处理代码冲突和重新构建。

收尾时先结束任务，保留/提交需要的改动，释放设备并清理，然后由 Worktree 的生命周期管理者删除目录。不会因为关闭 Codex 对话就自动证明外部资源已回收。

```powershell
& $agentctl release-device
& $agentctl cleanup --dry-run
& $agentctl cleanup
```

一个 Worktree 可能同时存在 Desktop Agent 固定 ID 和 Worktrunk 的 `worktrunk-<worktree-id>` 生命周期 runtime。删除前两者都必须清理。如果 pre-remove 报多个 active runtime，分别在该目录执行固定 Agent 和生命周期 cleanup，然后从另一个目录运行 `wt remove <branch> --foreground --no-delete-branch --yes`。Windows 当前 shell 不能停留在待删除目录中，否则 Git Worktree 可能已经解除但目录删除会因权限占用失败。

本次演练证明 `pre-remove` 会拒绝未清理的多个 runtime，分别 cleanup 后删除成功；设备租约、端口记录和构建锁都已释放。

Worktrunk hook 接入为可选自动化。使用 [桌面接入 Hook 示例](../examples/wt-desktop.toml) 合并到业务仓库 `.config/wt.toml`，不要覆盖已有 hook。该示例显式设置 F:\ar，并传回 agentctl 退出码；首次 hook 批准属于 Worktrunk 自身流程。pre-merge 的普通 check 不替代候选/组合验证。`check` 应调整为项目有效验证任务，基线已有失败不能伪报通过。

该新示例已检查 TOML 格式和退出码传播文本，手册八组 PowerShell 命令已通过语法解析；尚未针对这份新示例重跑创建、合并、删除的完整生命周期测试。

本文命令是操作模板，不构成执行证明。各轮实际安装、Worktree 创建和测试结果以对应实测报告为准；业务 AGENTS.md 需要审查后接入，不能用手册存在代替接入完成。
