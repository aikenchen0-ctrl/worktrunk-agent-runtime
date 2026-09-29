# Android 候选制品实测

本文件记录首轮结果。后续发现的收据误用、旧源码与候选混用、测试 JVM 共享锁、1086 项消费者测试结果及修复，见 [追加复核报告](review-parallel-aar-2026-09-26.md)。

## 范围

源项目为 `superAppAndroid` 的 `backSelfLanuchOKcnnOK`，基线提交为 `536735372568ddb1d2050b03e2b2744c4c2ad9cb`。
本轮使用真实 Android 插件、Kotlin 编译器、Maven 目录仓库、HTTP 下载和 Compose 单元测试。
两个测试 Worktree 是同一仓库的独立检出，不代表十几个 GitHub 仓库已经拆分。

- 提供方：`F:\sa6`，`univerge-heavy-drag-android`。
- 消费者：`F:\sa5`，`univerge-heavy-drag-compose`。
- 提供方测试提交：`c8e2e563065c2f9b8d61742b86c3b0ff295c3371`。
- 消费者测试提交：`8d5c844`。
- 候选版本：`0.1.0-dev.c8e2e563065c`。
- 候选事件：`20260926T082344-58ad23e1af8c`。
- AAR SHA-256：`70f9cdd2c5b5930d3d8db1620abfdd3ab38f9104b9059fe00f25c4ae9640aef0`。

## 实际结果

1. 提供方公开方法 `setStateListener` 改为 `observeState`，成功构建 Android release AAR。
2. 初版测试发布 POM 没有依赖，暴露了只上传 AAR 无法支持独立消费者的问题。
3. 测试发布同时生成 core JAR，并在 AAR POM 中声明其候选坐标；消费者通过 Maven 解析，未编译提供方源码。
4. 消费者旧调用在 `compileDebugKotlin` 失败，日志明确指出两处 `setStateListener` 无法解析。
5. 迁移调用后编译通过，Compose 单元测试 2 项通过、0 失败、0 跳过。
6. `validate-candidate.ps1` 实际下载候选文件、核验摘要、解析候选依赖、运行测试并生成事件收据。旧调用使事件成为 `failed`，迁移后成为 `passed`。
7. 最终使用 `-RerunTasks`，32 个 Gradle 任务实际执行。收据对应消费者测试提交。
8. 错误摘要使验证任务失败，测试不执行；构建锁正常释放。
9. 提供方与消费者两个 Worktree 同时强制构建成功，分别使用独立 Gradle 用户目录。
10. 使用 45 秒延时 Gradle 测试任务建立明确重叠窗口，同一 Worktree 的第二个 Agent 构建被拒绝，返回锁持有者和租约时间。
11. 生产源码声明快照识别出删除 1 个方法、新增 1 个方法；测试源码、方法正文和行号不再进入快照。
12. 对已发布的 `0.1.0-dev.c8e2e563065c` 再次发布失败，测试脚本拒绝覆盖，原 AAR 摘要保持不变。该检查不是远端仓库的原子防覆盖策略。
13. Rust 自动化回归测试 27 项全部通过，格式检查和 PowerShell 语法检查通过。

## 本轮修正

- 候选校验作为消费者测试的前置任务，在同一次 Gradle 调用中完成，避免两份构建收据脱节。
- 新快照使用 `kotlin-production-declarations-v2` 标记，拒绝旧格式基线，需从原基线提交重新生成。
- 本机 HTTP 仅在显式 `-AllowLoopbackHttp` 下允许 IP 回环地址；远端仍要求 HTTPS。
- 下载使用随机临时目录，避免并发消费者覆盖同名文件。
- 测试发布脚本拒绝已有坐标覆盖。

## 尚未证明

- 尚未完成十几个独立仓库、每个模块同时修改 API 的实测。
- 候选仓库是提供方运行时目录和临时回环 HTTP 服务，没有部署 Nexus，也没有远端仓库不可变策略验证。
- 测试发布脚本针对本项目 Android/core 依赖，POM 只包含此次必要 core 依赖，不是通用生产发布配置；第三方依赖、变体和资源仍需正式组件发布。
- 同一事件的 `app` 和 `univerge-accessibility` 尚未验证，所以不能判定集成就绪。
- 尚未触发 GitHub 升级 PR、跨机器通知、正式 ABI 校验、App 集成构建或设备运行测试。
- 轻量快照不能完整识别 Java、继承关系、属性、注解生成接口和二进制 ABI，不可代替正式兼容性工具。
- 临时 HTTP 服务测试后停止；消费者测试提交内的回环仓库配置只用于重放此测试，不能合并进业务主线。

## 重放入口

测试用初始化脚本为 `scripts/publish-test-aar.gradle`、`scripts/consume-test-aar.gradle` 和 `scripts/hold-test-build.gradle`。
生产消费者验证入口为 `scripts/validate-candidate.ps1`。它要求消费者构建真正读取候选版本属性并声明外部 Maven 依赖。
`GradleTask` 必须是 `ConsumerProject` 中的完整任务路径，例如 `:univerge-heavy-drag-compose:testDebugUnitTest`。

诊断证据保留在 `F:\ar\agents\aar-provider-live`、`F:\ar\agents\aar-consumer-live`、`F:\ar\events`，以及消费者模块的 Gradle 测试报告中。
