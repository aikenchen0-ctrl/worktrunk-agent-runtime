# Codex Desktop AAR Agent 依赖预下载使用手册

> 文档中的用户目录已匿名化；示例绝对路径须按实际机器替换。

适用项目：`superAppAndroid/backSelfLanuchOKcnnOK`。当前推荐快照为 `superapp-complete-v5`，对应基线提交 `536735372568ddb1d2050b03e2b2744c4c2ad9cb`。

本手册供多个 Codex Desktop Agent 在各自 Git Worktree 中复制使用。目标是让每个 Agent 在开始开发前获得相同的 Gradle、插件、业务依赖和测试运行时，避免十几个 Agent 同时从网络下载，也避免共享可写 Gradle 或 Maven 缓存造成锁竞争和 AAR 覆盖。

## 已预下载内容

| 分类 | 内容 |
| --- | --- |
| Gradle 发行包 | Gradle 8.10.2 |
| Gradle 依赖 | 主工程、Blinkvoice、Touch 的插件、业务依赖和传递依赖 |
| 测试运行时 | Robolectric Android 15、14、8、6、4.4 |
| 配置证据 | 三个构建根的配置解析报告 |
| 机器级工具链 | 本机已安装 JDK 17.0.20.1、Android 34/36、Build Tools 34/36、NDK、CMake、Ninja、ADB 和命令行工具；它们不打包进 Gradle 快照 |

快照有效载荷为 24,481 个文件、2,228,081,250 字节，约 2.08 GiB。`install-cache` 会把它复制到当前 Worktree 的私有可写目录。它减少网络下载，不减少每个 Worktree 的磁盘占用。

## Agent 启动前必须满足

1. 每个 AAR 使用独立 Git Worktree，并分配稳定且唯一的 `AGENT_ID`。
2. 所有 Agent、Worktrunk Hook 和协调任务统一使用 `AGENT_RUNTIME_ROOT=F:\ar`。
3. 在第一次执行 Gradle 前安装快照。目标缓存已有内容时，`install-cache` 会拒绝覆盖。
4. Windows Native 项目使用短 Worktree 路径和短运行时根目录，避免 Ninja 路径超过 260 字符。
5. 不使用全局 `~/.gradle`、`~/.m2`、`mavenLocal()`、动态版本或 `SNAPSHOT` 传递跨仓候选 AAR。

安装脚本默认核对依赖描述指纹，包括各级 `build.gradle`、`settings.gradle`、`gradle.properties`、版本目录、Wrapper 和构建逻辑。业务源码或文档提交可以继续使用 v5；依赖描述发生变化时必须生成新快照。只有维护者确认差异不影响依赖解析时才允许使用 `-AllowSourceDrift`，普通模块 Agent不应添加该参数。

## 首次安装

把以下变量中的 Worktree 路径、Agent ID 和验证任务替换为当前模块的实际值。整个命令块在一次 PowerShell 调用中执行。

```powershell
$runtime = 'F:\code\worktree\agent-runtime'
$worktree = 'F:\worktrees\aar-overlay-01'
$agentId = 'aar-overlay-01'
$agentctl = "$runtime\target-cache-validation\release\agentctl.exe"
$snapshot = "$runtime\.cache-snapshots\superapp-complete-v5"

& "$runtime\scripts\install-superapp-cache.ps1" `
  -SnapshotPath $snapshot `
  -AgentCtlPath $agentctl `
  -WorktreePath $worktree `
  -AgentId $agentId `
  -RuntimeRoot 'F:\ar' `
  -JavaHome "$runtime\.tools\msjdk17\jdk-17.0.20.1+1" `
  -AndroidSdkRoot 'C:\Users\developer\AppData\Local\Android\Sdk' `
  -VerifyTasks ':univerge-overlay:assembleDebug', ':univerge-overlay:testDebugUnitTest'

if ($LASTEXITCODE -ne 0) { throw '依赖快照安装或离线验证失败' }
```

脚本执行以下检查：

1. 校验快照清单、文件数量和总容量。
2. 准备当前 Worktree 的隔离运行目录。
3. 在构建锁内导入私有 Gradle 和 Maven 缓存。
4. 执行 `doctor`。
5. 使用 `--offline` 运行指定验证任务。
6. 输出当前 Agent 的实际隔离路径。

## 各模块建议验证任务

| Agent 负责范围 | 建议 `VerifyTasks` |
| --- | --- |
| App 或集成壳 | `:app:assembleDebug`, `:app:bundleRelease` |
| adbcore Native | `:adbcore:assembleDebug`, `:adbcore:assembleRelease` |
| Heavy Drag Android | `:univerge-heavy-drag-android:assembleDebug`, `:univerge-heavy-drag-android:compileDebugUnitTestKotlin` |
| Heavy Drag Compose | `:univerge-heavy-drag-compose:assembleDebug`, `:univerge-heavy-drag-compose:testDebugUnitTest` |
| Overlay | `:univerge-overlay:assembleDebug`, `:univerge-overlay:testDebugUnitTest` |
| Accessibility | `:univerge-accessibility:assembleDebug`, `:univerge-accessibility:compileDebugUnitTestKotlin` |
| Gesture Server | `:gesture-server:assembleDebug`, `:gesture-server:testDebugUnitTest` |

Blinkvoice 和 Touch 是独立构建根，不能直接使用主工程 Wrapper 的模块路径。它们按 [缓存分类与验证范围](superapp-cache-catalog.md) 中的固定 Gradle 8.10.2 Profile 执行；Touch 单元测试仍被现有源码错误阻塞。

## 日常使用

快照只需在空缓存中安装一次。以后每个命令块继续显式设置相同的运行环境，并通过 `agentctl run-gradle` 构建：

```powershell
$env:AGENT_RUNTIME_ROOT = 'F:\ar'
$env:AGENT_ID = 'aar-overlay-01'
$env:JAVA_HOME = 'F:\code\worktree\agent-runtime\.tools\msjdk17\jdk-17.0.20.1+1'
$env:ANDROID_SDK_ROOT = 'C:\Users\developer\AppData\Local\Android\Sdk'
$env:ANDROID_HOME = $env:ANDROID_SDK_ROOT
$agentctl = 'F:\code\worktree\agent-runtime\target-cache-validation\release\agentctl.exe'

& $agentctl run-gradle --agent-id $env:AGENT_ID -- ':univerge-overlay:testDebugUnitTest'
if ($LASTEXITCODE -ne 0) { throw '模块测试失败' }
```

正常开发可以联网解析新增依赖，因为每个 Worktree 的缓存独立。需要确认快照完整性时使用 `--offline`。不要把某个 Agent 下载后的私有目录复制给其他 Agent。

## 遇到新依赖或新版本

离线构建出现“缓存中没有可用版本”时，按以下顺序处理：

1. 核对依赖坐标、版本和仓库是否来自当前改动，排除源码编译错误或仓库配置错误。
2. 当前 Agent 可以在自己的私有缓存中联网构建以继续开发，不能写入共享 Gradle 或 Maven 目录。
3. 记录新增坐标、触发任务、构建根、Gradle/AGP/Kotlin/SDK 版本和成功日志。
4. 由指定的缓存维护任务使用全新 `AgentId` 和空种子缓存生成 `superapp-complete-v6` 等新快照。
5. 已有非空私有缓存不能直接覆盖。继续使用原缓存，或在新 Worktree 中安装新快照；需要清空时使用受控 `cleanup --purge-cache`，确认没有正在运行的构建后再安装。

同一个版本目录保持不可变。新依赖、新 Gradle Wrapper、新 SDK/NDK/CMake 组合或新的 Robolectric SDK 都生成新快照，不修改 v5。

## 常见失败

| 错误 | 原因 | 处理 |
| --- | --- | --- |
| `目标缓存已有数据，拒绝覆盖` | 已经运行过 Gradle或安装过快照 | 使用新的 Worktree；或确认无构建后受控清理缓存 |
| `Wrapper 配置与快照不一致` | 目标仓库的 Gradle Wrapper 已变化 | 生成匹配该 Wrapper 的新快照 |
| 离线模式找不到依赖 | 当前提交引入了 v5 未包含的新坐标 | 先在私有缓存联网验证，再登记并生成新快照 |
| Ninja 路径过长 | Worktree 或运行时目录太长 | 统一使用短 `AGENT_RUNTIME_ROOT` 和短 Worktree 路径 |
| Robolectric 仍访问网络 | 测试 JVM 没有通过外挂入口运行 | 使用 `agentctl run-gradle` 和测试隔离脚本，不直接执行系统 Gradle |
| `more than one device` | 绕过设备租约直接调用 ADB | 先 `acquire-device`，再使用 `agentctl adb` 或 `run-android-test` |

## 完成标准

Agent 只有同时满足以下条件，才能报告依赖环境就绪：

- `prepare`、`install-cache` 和 `doctor` 成功。
- 模块验证任务在 `--offline` 下成功。
- `agentctl env --format json` 显示私有 `GRADLE_USER_HOME` 和 `MAVEN_REPO_LOCAL` 位于统一状态根目录内。
- 没有把缓存、`local.properties` 或设备序列号提交到业务仓库。
- 没有把本机路径写入业务源码或 Gradle 配置；本手册中的本机安装示例不属于构建配置。
- 后续 API 候选仍使用不可变 Maven 坐标和消费者验证流程；缓存成功不等于跨 AAR 兼容性通过。
