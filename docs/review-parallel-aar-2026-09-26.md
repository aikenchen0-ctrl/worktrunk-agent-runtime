# 并行 AAR 协作复核与新增实测

## 结论和范围

当前仍未完成十几个独立仓库、每个 AAR 同时修改 API 的完整验收。本轮在指定分支派生的 `F:\sa5` 和 `F:\sa6` 上继续真实 Gradle 测试，复核了消费者确认、依赖混用、发布元数据和测试 JVM 隔离。

指定基线：`536735372568ddb1d2050b03e2b2744c4c2ad9cb`。提供方提交 `c8e2e563065c2f9b8d61742b86c3b0ff295c3371`，消费者提交 `8d5c844b5dc2895db3c7c64d3d311f1cfe7ceec0`，本轮没有修改这些业务提交。

项目 settings 包含 11 个项目名，但不是 11 个独立 AAR：有 JVM 库、App、基准测试和没有本地构建文件的入口。不能把此数量报告为十几个独立 AAR Agent 已经验收。

## 修复和证据

| 问题 | 修复 | 实测结果 |
| --- | --- | --- |
| 成功的 `help` 收据加环境摘要可能被误认为候选通过 | 必须有本次 Gradle 解析和实际测试证据 | 真正 `help` 成功后，`passed` 被拒绝；日志 `gradle-96160-20260926T150908Z` |
| 旧成功收据可用于新一轮确认 | 绑定验证编号、消费者、提交、时间、Agent 和 Worktree | 新建验证后使用旧收据，被拒绝 |
| 多 Agent 覆盖同一消费者状态 | 原子状态锁、两小时验证租约、持有者检查 | 自动化验证覆盖其他 Agent、过期、旧状态；不允许 `received` 覆盖已有结果 |
| 下次构建覆盖历史收据 | 独立 `receipts/<run_id>.json` 和确认内嵌快照 | 后续 accessibility 构建失败后，Compose 通过证据仍保留 |
| 消费者同时引入旧源码和新 AAR | 校验实际解析图，识别同 group/name 的项目与外部模块混用 | accessibility 实际解析被拒绝；日志 `gradle-14012-20260926T151008Z` |
| 仅手工附加 AAR，POM 丢失第三方依赖 | 从 AGP release component 发布 | 新测试版本 `.metadata1` 同时包含 core、Kotlin stdlib、AndroidX core-ktx，生成 `.module`；日志 `gradle-99368-20260926T151242Z` |
| Robolectric 测试 JVM 另行下载，并使用用户主目录锁 | 自动注入测试 JVM Maven 路径、user.home 和 temp | Android framework JAR 实際落在 Worktree 的 `maven-local/org/robolectric`；Compose 两项测试通过 |
| 网络/测试挂起长期占锁 | 受控命令超时，终止本次进程树 | 提供方设置 8 秒超时后约 9.7 秒返回失败，锁释放；并行 Compose 验证继续成功 |
| 命令行可重定向共享缓存或其他项目 | 拒绝 `-g`、`-p`、项目缓存和构建入口重定向等参数 | 回归覆盖短参数、完整参数及长参数缩写 |
| 同事件 ID 并发导入可能覆盖 | 导入写入加互斥锁 | 12 个并发线程导入同 ID 的不同内容，只有 1 个成功；这是状态并发测试，不是 12 个 AAR 构建 |
| 集成清单非字符串项被忽略 | 非法项直接失败 | `[null, 12]` 不再被视为全部就绪 |

## 真实业务测试

Compose：真实候选版本 `0.1.0-dev.c8e2e563065c`，事件 `20260926T082344-58ad23e1af8c`，AAR SHA-256 `70f9cdd2c5b5930d3d8db1620abfdd3ab38f9104b9059fe00f25c4ae9640aef0`。隔离 JVM 首轮构建约 51 秒，最终脚本复验约 20 秒；两次均为测试 2 项、失败 0、跳过 0。最终日志 `gradle-95716-20260926T154158Z`，确认包含 `evidence_version: 2`。

accessibility：统一 Android/core 依赖来源后，生产代码和测试代码编译成功。首次测试出现下载等待，线程栈位于 Robolectric `MavenArtifactFetcher`，保存于 `F:\ar\robolectric-test-hang-20260926.log`。停止的是本次测试进程，随后 Gradle 正常报告失败并释放锁。

补上测试 JVM 隔离和显式 Maven 镜像后，完整运行在约 43 秒内结束：227 个测试套件、1086 项测试、43 项失败、0 项跳过。日志 `gradle-61984-20260926T153010Z`，完整 XML 已保存在 `F:\ar\evidence\accessibility-full-20260926`，避免后续对照构建覆盖。不能报告消费者兼容通过。

基线对照先用手工 JUnit 运行，但第二组缺少类路径，因此未采用其结论。随后通过 Gradle 完整测试类路径，对原始分支源码运行 `FloatingChatAiVoiceEntryTest` 和 `FloatingChatMessageUiContractTest`：149 项测试、相同的 6 项失败，日志 `gradle-7216-20260926T153850Z`。这是使用未变的测试类读取原始源码的对照，不是完整基线重新编译。accessibility 源码与测试相对指定基线无差异；其余 37 项尚未逐项归因。

外挂最终 Rust 回归 34 项通过，格式检查、release 编译和 PowerShell 语法检查通过。未进行新的真机安装测试，未修改或推送业务主分支。

默认 PATH 优先命中 Windows Terminal 的 `wt.exe`，doctor 正确失败。修复诊断逻辑以检查首个实际命中路径后，在仅本次 shell 优先设置已安装 Worktrunk 路径的条件下，doctor 所有检查通过；没有修改全局 PATH。两台设备在线、一台未授权。回环 Maven 服务已停止，两个测试 Worktree 构建锁已释放，`F:\sa4` 原有修改保留。

最终原候选事件仍为 Compose `passed`、app/accessibility `pending`、`ready_for_integration: false`。新 `.metadata1` 的 accessibility 测试失败不能写成原候选事件通过。

本轮新增测试发布版本为 `0.1.0-dev.c8e2e563065c.metadata1`；它验证发布元数据修复，未伪造为原候选事件的版本。旧版本没有覆盖。

## 对原方案的校正

- Gradle 官方说明依赖缓存使用文件锁支持能互相通信的进程并发访问。独立 Gradle 用户目录是本项目降低共享状态和锁争用的策略，不能说官方禁止所有多进程共享。[Gradle 文档](https://docs.gradle.org/current/userguide/dependency_caching.html)
- `adb reverse` 的监听端在选定设备上；两台不同手机用相同设备端口不会自动互相覆盖。宿主服务需要分别监听不同本机端口，并正确绑定设备映射。[ADB 手册](https://android.googlesource.com/platform/packages/modules/adb/+/refs/heads/main/docs/user/adb.1.md)
- 接口可以破坏性演化；旧稳定制品可以继续供未迁移消费者使用，不必强迫新版本永远保留旧方法。
- 新实现尚未发布时，可用独立契约桩或候选分支提前开发，但最终仍需真实制品重验。不能宣称完全没有工程方案支持提前并行。
- “一个 AAR 一个 Agent”是降低冲突的组织选择，并非 Git 的强制约束；同模块多个 Agent 仍可在不同 Worktree 修改，合并冲突和模块发布必须另行协调。
- Robolectric 4.16.1 源码确认其下载锁位于测试 JVM 的 `user.home`，仓库优先读取测试 JVM 的 `maven.repo.local`；只设置 Gradle 用户目录不覆盖这一层。[Robolectric 源码](https://raw.githubusercontent.com/robolectric/robolectric/robolectric-4.16.1/plugins/maven-dependency-resolver/src/main/java/org/robolectric/internal/dependency/MavenDependencyResolver.java)

## 尚未对齐的验收项

1. 多仓拆分、每个模块破坏性 API 修改、十几个实际 AAR 并行构建、稳定组合回滚尚未完成。
2. 通知图仍靠项目源码扫描和消费者登记，尚无各独立仓库实际解析图的持续汇总。事件缺少迁移文档、语义变化、基线组合的完整机器协议。
3. 通知存储不等于 Codex 自动接收或执行。尚无跨任务推送、离线补投、合并去重、升级 PR 的完整连接器。
4. 候选事件 `passed` 只证明所指定测试运行成功；不等于 PR 已合并、消费者主分支已升级或集成 App 可运行。
5. 轻量 API 扫描缺少正式 ABI、Java、资源、Manifest、JNI 和行为兼容性覆盖。
6. 远端不可变 Maven 仓库、完整候选依赖组合、可信 CI 证据导入、App release 集成和真机矩阵尚未完成。
7. 运行时仍是同账户下的协作控制器，无法防止 Agent 直接修改状态文件、绕过入口或让任意 Gradle 脚本写外部目录。
8. 测试日志存在 JVM 到 Windows PowerShell 的中文乱码；编码尝试无效且引入 CLIXML 后已撤回，未掩盖此限制。

2026-09-27 的后续组合验证已移入独立模块 `src/integration.rs`，实测范围和验收结果见 [组合验证报告](integration-validation-2026-09-27.md)。尚未在完整 App 集成壳和真实十模块候选集合上完成验收。

## Agent 使用规则

提供方在自己的仓库更新版本化接口和迁移说明，不让多个 Agent 写同一个共享实时文档。事件交接提交、候选坐标、摘要、兼容性变化和消费者范围；消费者各自提交迁移并由验证脚本产生确认。通知只是提醒，收件箱和经过验证的状态用于补读和审计。

完整提示模板见 `examples/AGENTS.md`。消费者正式验证入口为外挂安装目录内的 `scripts/validate-candidate.ps1`，不要求每次对话都重新生成所有接口文档，只要求与契约相关的改动同步其版本化说明。
