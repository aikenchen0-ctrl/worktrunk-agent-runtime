# agent-runtime

> 文档中的用户目录已匿名化；示例绝对路径须按实际机器替换。

**发布状态：0.1.0 预览版准备中，仅承诺本机控制器范围。** 安装、打包、升级边界及远端发布门槛见 [发布指南](docs/release-guide.md)；安全边界见 [SECURITY.md](SECURITY.md)。

`agent-runtime` 是独立于 Worktrunk 的本机 Agent 运行时控制器。它通过 `agentctl` 为每个 Git Worktree 隔离 Gradle 用户目录和 Maven 本地仓库，互斥写入型构建，分配 Android 设备租约，并记录端口、运行状态和审计事件。它不修改 Worktrunk 核心源码，不含 AAR 业务代码。

使用 Codex Desktop 在多个 Worktree 工作时，先阅读 [桌面接入流程](docs/desktop-quickstart.md)。十几个 AAR Agent 首次导入预下载依赖时使用 [Agent 依赖预下载手册](docs/codex-agent-cache-guide.md)，并把 [依赖预下载 AGENTS 片段](examples/AGENTS-cache.md) 合并到业务仓库规则中。

本机独立仓库通过 [消费者路由](docs/consumer-routing.md) 区分同名模块。当前真实构建范围、修复和未完成项见 [2026-09-27 并行 API 实测报告](docs/parallel-api-validation-2026-09-27.md)。组合通过不等于全部消费者迁移或允许发布。

跨仓正式发布、Codex 任务自动通知和升级 PR 尚未形成自动闭环；实现边界与验收条件见 [跨仓自动化状态](docs/cross-repo-automation-status.md)。本机收件箱和 CI 模板不能视为这些能力已经接通。

候选发布、消费者升级和协作协调可由三个按需 Skill 复用现有工具完成，源文件位于 `skills/`，用法见 [Codex Skill 接入说明](docs/codex-skills-guide.md)。Skill 不替代锁、制品不可覆盖、真实验证收据或用户授权，也不代表后台自动化已上线。

## Windows 安装

需要 Rust 工具链、Git、PowerShell；Android 构建另需 JDK、Android SDK 与 ADB。第一次在本目录构建可执行文件：

```powershell
cargo build --release --locked
.\scripts\package-release.ps1 -SkipBuild
```

验证发布包后，运行包内 `scripts/install.ps1`，默认安装到 `%LOCALAPPDATA%\agent-runtime`，同时安装 Hook 适配脚本。安装器拒绝覆盖旧安装；不会修改全局 PATH、运行状态或用户 Skill。详见发布指南。正式部署建议使用组织的签名与分发流程。

在目标 Worktree 中初始化并检查：

```powershell
$agentctl = Join-Path $env:LOCALAPPDATA 'agent-runtime\bin\agentctl.exe'
& $agentctl prepare
& $agentctl doctor
& $agentctl run-gradle -- assemble
```

`prepare` 默认拒绝主 Worktree。只有确实要在主 Worktree 操作时才传 `--allow-primary`。`run-gradle` 自动为受控子进程设置环境，不要求先执行 `env`。`agentctl env --format powershell` 只输出当前 Shell 环境脚本，不会永久修改 Codex Desktop 后续命令的环境。

## 运行时目录

Windows 状态默认写入 `%LOCALAPPDATA%\agent-runtime`：

含 Native/CMake/Prefab 的项目在 Windows 上可能遇到 Ninja 的 260 字符路径限制。此时可在所有 Agent 和 Worktrunk Hook 的启动环境中统一设置短的绝对路径，例如 `$env:AGENT_RUNTIME_ROOT = 'F:\ar'`。不能只为某一个 Agent 设置：设备租约、端口和构建锁都登记在该根目录下，不同根目录之间不会互斥。切换根目录会创建一套新的状态，不会自动迁移旧租约或缓存；切换前应先停止构建并释放旧租约。

```text
agent-runtime/
  worktrees/<worktree-id>/
    worktree.json
    locks/build.json
    gradle-user-home/
    maven-local/
  agents/<agent-id>/<worktree-id>/
    metadata.json
    worktree.env
    ports.json
    device-lease.json
    locks/device-operation.json
    logs/
    temp/
  devices/<serial>.json
  ports.json
  events.jsonl
```

Worktree ID 基于 Git 的专属 Worktree 管理目录计算，切换分支或重启 Codex 不会换一份缓存。同一 Worktree 内不同 Agent 共用 Gradle/Maven 缓存和构建锁；每个 Agent 有自己的日志、临时文件、设备租约和清理状态。并行 Agent 必须由启动器提供稳定且唯一的 `AGENT_ID`；Codex Desktop 不会自动分配该值。

`run-gradle` 总是设置独立 `GRADLE_USER_HOME` 并传入 `-Dmaven.repo.local=<worktree runtime>`。项目若配置 `mavenLocal()`，其解析位置也由该 JVM 属性隔离。跨仓库候选 AAR 仍应发布至唯一、不可覆盖的远端坐标；本地 Maven 临时库只用于该 Worktree 内验证。

可用 `AGENT_MAX_WORKERS` 限制 Gradle worker 数、`AGENT_MAX_HEAP` 设置 JVM 最大堆；Windows 还可设 `AGENT_CPU_PERCENT`（1–100）限制整个 Gradle 进程树 CPU，及 `AGENT_MEMORY_LIMIT_MB`（至少 256）限制其总内存。Windows 配额由 Job Object 强制执行；Linux/macOS 目前只支持 Gradle worker/JVM heap 设置，进程树的 OS 配额尚未实现。

`AGENT_COMMAND_TIMEOUT_SECONDS` 限制受控命令耗时，默认 7200 秒，范围 1–86400；超时回收当前子进程树并保留失败收据。Gradle 的 JVM 测试自动使用 Worktree 内的 Maven 仓库、测试用户目录和临时目录，避免 Robolectric 在用户主目录共享下载锁。可用 `AGENT_TEST_MAVEN_URL` 显式选择无凭证的 HTTPS 测试依赖镜像。测试不能依赖原用户主目录中的隐式配置；凭证应通过受控环境提供。

## 样例项目依赖预热

`scripts/warm-superapp.ps1` 为 `superAppAndroid/backSelfLanuchOKcnnOK` 的三个实际构建根统一预热依赖：主工程、`blinkvoice-visual-sdk` 和 `univerge-touch`。脚本使用主工程 Gradle 8.10.2 Wrapper 预热 App、AndroidTest、Benchmark、主要 AAR 和三套 Robolectric 测试，再用同一固定 Gradle 8.10.2 发行版预热 Blinkvoice 的 Debug、Release、50 个单元测试，以及 Touch 的 Debug、Release 和发布元数据。快照按 `gradle-dependencies`、`gradle-distribution`、`maven-test-runtime`、`configuration-reports` 分类；Robolectric framework JAR 会随快照进入每个 Worktree 的私有 Maven 目录。

种子 Gradle User Home、只读缓存和项目源码必须使用三个互不包含的绝对路径。示例：

```powershell
$env:AGENT_RUNTIME_ROOT = 'F:\ar'
$env:JAVA_HOME = 'F:\code\worktree\agent-runtime\.tools\msjdk17\jdk-17.0.20.1+1'
$env:ANDROID_HOME = Join-Path $env:LOCALAPPDATA 'Android\Sdk'
.\scripts\warm-superapp.ps1 `
  -ProjectPath 'F:\sa5' `
  -SnapshotPath 'F:\ar\cache-snapshots\superapp-complete-v5' `
  -AgentCtlPath 'F:\code\worktree\agent-runtime\target\release\agentctl.exe' `
  -AgentId 'superapp-cache-seed'
```

预热脚本要求 `AgentCtlPath` 是绝对路径，因为它会切换到样例项目目录。每次生成新快照必须使用新的 `AgentId`，脚本会拒绝含历史 Gradle、Maven 或报告数据的种子缓存。每个使用者仍通过 `agentctl prepare` 获得自己的可写 Gradle 用户目录和 Maven 仓库，然后在新建、尚未构建的 Worktree 中运行 `agentctl install-cache --snapshot <快照绝对路径>`。安装命令在构建锁保护下核对 Wrapper 摘要，再把发行包与下载依赖复制到该 Worktree 的私有目录；目标缓存已有数据时会拒绝覆盖。项目 `.gradle`、构建输出、Maven/Robolectric 缓存继续隔离。Android SDK 平台、构建工具和模拟器映像属于 SDK Manager 管理的机器级工具链，不能靠 Gradle 依赖缓存预热。

当前优先使用 `agent-runtime/.cache-snapshots/superapp-complete-v5`，有效载荷 24,481 个文件、2,228,081,250 字节，约 2.08 GiB。它包含 Gradle 8.10.2 发行包、三个构建根的插件与依赖、配置扫描报告，以及 Android 15、14、8、6、4.4 五套 Robolectric 运行时。v5 已安装到全新 detached Worktree 的空私有缓存，并在 Gradle `--offline`、Robolectric 仓库指向不可访问地址的条件下重新执行 234 个任务；四种 ABI 的 Native Debug/Release、App Release Bundle 和 Overlay Robolectric 测试均成功。v4 及更早快照仅用于历史追溯。

JDK、Android SDK、Build Tools、NDK 和 CMake 仍是机器级工具链，不包含在 Gradle 快照内；版本和安装包摘要记录在 `.cache-snapshots/toolchain-inventory.json`。Touch 仓库声明的 Wrapper 8.7 与 AGP 8.7.3 不兼容，外挂服务暂时使用固定 8.10.2 启动器；其单元测试被现有 Java 包名和可见性错误阻塞，和依赖下载无关。分类目录、离线证据和独立构建根命令见 `docs/superapp-cache-catalog.md`。

若 `doctor` 报 Windows Terminal 的 `wt.exe` 冲突，先用 `where.exe wt` 检查顺序。使用已确认的 Worktrunk 完整路径，或只为当前 shell 把其目录放到 `$env:PATH` 前面；不必删除 Windows Terminal。诊断依据首个命中路径，两个程序同时安装不必然构成冲突。

## 命令

- `agentctl prepare [--allow-primary]`：识别 Git Worktree 并创建隔离运行目录。
- `agentctl env [--format shell|powershell|json]`：输出运行环境，不含凭证。
- `agentctl install-cache --snapshot 快照目录`：持有 Worktree 构建锁，将已核验快照复制到当前私有 Gradle 用户目录；拒绝覆盖已有缓存。
- `agentctl candidate-version --base 版本`：根据当前 Git 提交生成不可覆盖的 `版本-dev.<提交短 SHA>` 候选版本。
- `agentctl dependency-audit [--format text|json] [--enforce]`：审计 `mavenLocal()`、`SNAPSHOT`、文件型 AAR 和同仓 `project(...)` 依赖；`--enforce` 发现问题时返回非零。
- `agentctl dependency-graph [--format text|json]` / `affected-consumers --provider 模块名`：输出同仓依赖边和直接消费者。
- `agentctl contract-snapshot [--format text|json] [--module 模块目录] [--output 文件]`：扫描指定 AAR 模块的 Kotlin 公开声明并保存可比较的契约快照；基线和当前快照必须使用相同模块目录。
- `agentctl contract-diff --baseline 文件 [--module 模块目录] [--format text|json] [--enforce]`：比较同一模块的契约基线；删除公开符号时标记破坏性变更，`--enforce` 返回非零。
- `agentctl contract-event --provider 模块名 --base 版本 [--module 模块目录] [--baseline 文件] [--artifact-url URL] [--artifact-sha256 SHA256] [--artifact-coordinate 坐标] [--format text|json]`：创建包含候选版本、契约哈希、不可变 AAR 地址、SHA-256、Maven 坐标和受影响消费者的可重放事件。
- `agentctl event-inbox --consumer 模块名 [--format text|json]` / `event-ack --event 事件ID ... [--receipt 收据文件]`：读取候选事件并记录消费者验证状态。
- `agentctl event-import --file 事件JSON [--format text|json]`：导入本地可读的候选事件文件；不负责跨机器传输、稳定仓库身份映射或可信 CI 收据导入。
- `agentctl event-status [--event 事件ID] [--format text|json]`：汇总候选事件的消费者确认状态；只有所有受影响消费者通过后才显示“集成就绪”。
- `agentctl status [--json]`：查看当前运行时、构建锁和设备租约。
- `agentctl lock-build [--timeout 秒] [--lease 秒] [--owner 名称]` / `unlock-build [--token 令牌]`：管理可审计构建锁。
- `agentctl run-gradle [--allow-clean] -- <Gradle 参数>`：在锁保护下运行 Worktree Wrapper，记录输出、退出码和耗时。
- `agentctl acquire-device (--serial 序列号 | --any) [--wait 秒] [--lease 秒] [--format text|json]`：租用设备；`--any` 会在所有在线设备中原子尝试。
- `agentctl heartbeat-device` / `release-device [--token 令牌]`：续租或释放。
- `agentctl run-android-test -- <Gradle 测试任务>`：验证租约并运行测试，设置租约序列号到 `ANDROID_SERIAL`，并追加 `--no-daemon` 避免复用旧设备环境；测试期间自动续租且持有构建锁和设备操作锁。
- `agentctl adb -- <ADB 子命令和参数>`：强制使用租约序列号执行 ADB 命令，拒绝覆盖设备选择或控制共享 ADB Server；操作期间续租并持有设备操作锁。
- `agentctl port --name 用途 [--preferred 端口] [--base 端口] [--range 数量]`、`ports [--json]`：分配和查询本机端口。
- `agentctl cleanup [--dry-run] [--force] [--purge-cache] [--worktree-branch 分支 | --worktree-path 绝对路径]`：清理当前 Agent 或指定 Worktree 的租约、端口记录和状态；清理 Worktree 时需从其有效 metadata 定位目标。
- `agentctl doctor [--format text|json]`：检查工具链并回收经 PID、进程启动时间、令牌和租期核验的孤儿锁/租约；发现 `FAIL` 时返回非零退出码。

构建锁和状态互斥锁均通过同目录原子创建。释放时比较令牌；发现锁内容未知或状态改变时失败关闭，不会删除未知锁。清理目标在规范化后必须位于当前 Agent Runtime 根目录。

## Worktrunk 接入

把 [examples/wt.toml](examples/wt.toml) 中的内容复制到 AAR 仓库的 `.config/wt.toml`。Worktrunk 项目 Hook 使用顶层 `pre-start`、`pre-merge`、`pre-remove` 和 `post-remove` 键，并以命名命令映射配置。`pre-remove` 在 Worktree 尚存在时停止 Daemon 并回收生命周期 runtime；`post-remove` 以仓库内唯一分支名从 runtime metadata 定位目标，兼容目录已被删除的情况。Worktrunk 首次运行项目 Hook 时需要用户批准。

Worktrunk Hook 使用 PowerShell 调用固定安装路径，因此不依赖 Git Bash 或 Codex Desktop 的临时 PATH。PowerShell 转发脚本位于 `scripts/`。

## 契约变更和 Agent 通知

接口文档不能代替编译验证。提供方 Agent 应提交接口和行为说明，生成契约快照，先发布候选制品并确认可下载及摘要一致，再创建可供消费者验证的候选事件；消费者 Agent 通过收件箱读取事件，使用固定候选版本编译和测试，再确认通过或失败。发布前的接口预告不能充当制品已就绪的证据；当前尚未实现发布事务与事件自动衔接。

建议每个 AAR 仓库保留以下可审查规则文件：

```text
docs/api/README.md
docs/api/compatibility.md
config/dependents.yml
```

这些文件只描述接口、迁移和依赖关系，不保存某个 Agent 的临时状态。候选事件、收件箱和确认状态统一保存在 `AGENT_RUNTIME_ROOT` 外部目录，由外挂服务原子写入，避免多个 Worktree 争用共享文档。

提供方流程：

```powershell
agentctl contract-snapshot --module univerge-heavy-drag-core --format json --output "$env:AGENT_RUNTIME_ROOT\baselines\core-baseline.json"
agentctl contract-diff --module univerge-heavy-drag-core --baseline "$env:AGENT_RUNTIME_ROOT\baselines\core-baseline.json" --enforce
agentctl contract-event --provider univerge-heavy-drag-core --module univerge-heavy-drag-core --base 0.1.0 --baseline "$env:AGENT_RUNTIME_ROOT\baselines\core-baseline.json" --artifact-url https://maven.example.invalid/com/example/core/0.1.0-dev.sha/core-0.1.0-dev.sha.aar --artifact-sha256 <64位SHA256> --artifact-coordinate com.example:core:0.1.0-dev.sha --format json
```

消费者流程：

```powershell
agentctl event-inbox --consumer univerge-heavy-drag-compose --format json
agentctl event-ack --event <事件ID> --consumer univerge-heavy-drag-compose --status received
.\scripts\validate-candidate.ps1 -EventFile <事件文件路径> -Consumer univerge-heavy-drag-compose -GradleTask ':univerge-heavy-drag-compose:testDebugUnitTest' -CandidateProperty HEAVY_DRAG_ANDROID_VERSION -ConsumerProject ':univerge-heavy-drag-compose' -Configuration debugCompileClasspath
agentctl event-status --event <事件ID> --format json
```

事件文件和 `events.jsonl` 是本机可重放记录，Agent 可通过 `event-inbox` 补读；确认状态用于判断哪些消费者尚未验证。当前尚未实现 Codex 消息、CI 评论或桌面通知适配器，也没有自动投递和重试机制。后续接入通知时，消息送达与消费者验证通过必须分别记录。

`event-ack --status passed` 不是人工勾选。正式验证前提交消费者迁移；验证脚本申请两小时验证租约并生成唯一编号。确认必须匹配当前 Agent、Worktree、提交、事件、本次编号、实际解析制品和实际执行测试。普通 `run-gradle -- help` 的成功收据不能确认通过；旧收据、其他消费者收据及验证期间源码变更也被拒绝。旧版 `passed` 在查询中显示 `needs_revalidation`。确认内嵌收据快照，每次构建另存 `receipts/<运行编号>.json`，后续构建不会替换历史证据。

验证脚本要求 `CandidateProperty`、`ConsumerProject` 和 `Configuration`。`GradleTask` 必须是该消费者项目内 JVM `Test` 任务的完整路径，例如 `:library:testDebugUnitTest`；聚合 `check`、`assemble` 和设备测试暂不支持此证据协议。它在同一次 Gradle 调用中校验实际依赖图、JAR/AAR 摘要和测试结果，支持直接及传递依赖，拒绝同模块 `project(...)` 与 Maven 制品混用。测试必须重新执行且至少一项未跳过，编译可复用缓存；`-RerunTasks` 可进一步强制所有任务重跑。

[examples/candidate-validation.workflow.yml](examples/candidate-validation.workflow.yml) 和 [scripts/validate-candidate.ps1](scripts/validate-candidate.ps1) 是消费者验证接入模板，尚未通过跨机器端到端验收。当前模板仅接受 `workflow_dispatch`，输入 `event_file` 是 checkout 后本地可读的事件 JSON 路径，`agentctl_url` 必须配套提供 `agentctl_sha256`。事件路由仍绑定本机仓库绝对路径，不能直接把开发机事件交给 GitHub runner 后期待验证通过；必须先补齐稳定仓库身份、安全分发和可信 CI 收据导入。模板不发布候选、不创建升级分支或 PR，也不通知 Codex 任务。

`superAppAndroid/backSelfLanuchOKcnnOK` 当前是一个包含多模块的仓库，其中 `univerge-heavy-drag-core` 发布 JAR，`univerge-heavy-drag-android` 等 Android 模块才产出 AAR。真实 Gradle 验证已证明：继续使用 `api(project(":univerge-heavy-drag-core"))` 的消费者会被明确拒绝；对已有外部 JAR (`junit:junit:4.13.2`) 和 AAR (`androidx.core:core-ktx:1.15.0`) 的坐标及摘要校验可通过。这不等于十几个独立仓库的候选 AAR 已经发布、升级和组合测试完成。

消费者需要让 Gradle 属性真正选择外部候选坐标。例如可使用 `val coreVersion = providers.gradleProperty("HEAVY_DRAG_CORE_VERSION").get()` 和 `api("com.paifa:univerge-heavy-drag-core:$coreVersion")`。当前仅实测 Worktree 的 Compose 消费者完成接入，业务原分支仍使用项目依赖。

GitHub Actions 的 checkout 目录是临时主 Worktree，因此模板显式使用 `prepare --allow-primary`；本机 Codex Agent 仍应使用独立 Worktree，不能把该参数作为日常开发默认值。

进一步的真实 API 改名、候选 AAR 发布、消费者失败与迁移通过、事件收据和并行互斥测试见 [2026-09-26 实测报告](docs/real-aar-validation-2026-09-26.md)。本机重放可显式使用 `-AllowLoopbackHttp`，远端 CI 不应启用它。旧契约基线需使用当前版本从原提交重新生成。

后续追加的收据误报修复、Robolectric 隔离、第二消费者 1086 项测试和剩余缺口见 [并行 AAR 复核报告](docs/review-parallel-aar-2026-09-26.md)。

多个提供方同时变化时，不要把多个候选事件分别通过就直接合并。先为每个事件完成消费者验证，再由集成壳工程固定一组候选坐标执行组合构建和测试；`event-status` 只能证明单个候选事件的消费者验证完成，不能替代组合级集成测试。

组合验证使用外部运行时索引，不修改仓库中的集成清单：

```powershell
agentctl run-integration --manifest .config/integration.json -- -PANDROID_MODULE_VERSION=固定候选版本
agentctl integration-status --manifest .config/integration.json --format json
```

清单格式见 [integration-manifest.json](examples/integration-manifest.json)，必须指定项目、实际可解析配置和 JVM 测试任务，构建任务可选。运行前，清单中的每个事件必须有全部登记消费者的有效收据。运行时校验整组候选的实际坐标和摘要、拒绝项目源码混用，并强制本次测试执行；实际解析制品列表随证据归档。命令行只能额外传版本属性、`--offline`、`--stacktrace` 或 `--rerun-tasks`，不能覆盖任务或跳过校验。

控制器在外部状态目录保存清单、事件内容、消费者确认和校验脚本摘要绑定的输入快照，以及当前提交的独立构建收据。上述输入或提交变化时旧证据不可复用。`combination_verified: true` 仅表示指定清单验证通过；`release_approved` 始终为 false，不代表已合并、已发布或真机验收完成。当前还未锁定和重新验证所有第三方依赖的后续漂移，正式发布仍需可信 CI 和完整依赖锁。

已完成真实 Android AAR 与传递 core JAR 的 Compose 组合构建、故意错版本拒绝及恢复通过，详见 [组合验证实测](docs/integration-validation-2026-09-27.md)。调整后的总体目标和验收标准见 [需求基线 v2](docs/requirements-v2.md)。

Codex 可直接在已打开的 Worktree 中执行 `agentctl run-gradle -- assemble`、`agentctl run-android-test -- connectedAndroidTest` 和 `agentctl adb -- install app-debug.apk`。设备序列号从该 Worktree 的有效租约注入；安装、卸载、启动、截图和日志等写操作必须走 `agentctl adb`，避免直接调用 ADB 绕过租约。环境由 `agentctl env` 查询。

## 测试

```powershell
cargo fmt --check
cargo test --offline
cargo run -- --help
```

测试覆盖主 Worktree 拒绝、Worktree 身份和目录隔离、构建锁互斥/令牌校验、假 ADB 输出筛选、假设备租约互斥、端口与堆参数校验、PowerShell 转义和清理路径边界。本机是否连接真实设备不影响假 ADB 测试；真实设备验证应单独运行只读的 `agentctl adb -- get-state`，不能作为 CI 前提。

## 明确边界

本工具解决本机 Worktree 运行状态和资源互斥。它不能单靠接口文档自动同步跨仓库 AAR。候选制品发布、依赖图通知、消费者升级 PR、API 兼容检查和集成版本验证应由 GitHub Actions 与 Maven 制品仓库完成。

进一步限制与扩展方向见 [已知限制](docs/known-limitations.md)。
