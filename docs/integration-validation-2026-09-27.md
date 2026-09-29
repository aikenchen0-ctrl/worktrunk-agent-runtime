# 组合验证实测

## 需求落实

需求基线见 [requirements-v2.md](requirements-v2.md)。本轮完成“单候选通过后仍必须验证实际组合”的本机执行闭环。未修改 Worktrunk 核心，未修改业务测试 Worktree 的源码和提交。

`run-integration` 的清单指定项目、解析配置、测试任务及可选构建任务。控制器在锁内生成本次独立输入和脚本，使用原有 Gradle 运行器执行，归档独立收据；不会修改仓库清单。实际制品坐标、摘要及完整选中制品列表由 Gradle 返回，所有候选必须同时匹配，测试须本次执行。

输入摘要包含清单、事件内容、消费者确认和校验脚本版本；源码提交由构建收据绑定。新验证开始后状态变为 running，中断不会保留旧通过状态。失败、脏工作区、换提交、确认变化或清单变化不能复用旧结果。

## 实测范围

- 工程来源：`superAppAndroid/backSelfLanuchOKcnnOK`。
- 消费者：`F:\sa5` 的 Compose 模块，提交 `8d5c844b5dc2895db3c7c64d3d311f1cfe7ceec0`。
- 组合：Android AAR 与其传递 core JAR，版本均为 `0.1.0-dev.c8e2e563065c.metadata1`。
- AAR SHA-256：`70f9cdd2c5b5930d3d8db1620abfdd3ab38f9104b9059fe00f25c4ae9640aef0`。
- JAR SHA-256：`4e37156db610e0170663bafe80c0410f6d6daeae11b47db584eb62de5be31acc`。
- 测试事件：`integration-review-20260927-android`、`integration-review-20260927-core`。它们显式限定 Compose 实验范围，不替代原始全量消费者事件，不代表所有模块都改过 API。

## 已观察结果

1. 原始事件 `20260926T082344-58ad23e1af8c` 仍有 app/accessibility 未验证，组合执行在启动 Gradle 前拒绝。
2. 两个实验事件分别用真实 Gradle 解析和测试得到 passed，各执行两项 Compose 测试。
3. 两个单候选都 passed 后，没有组合收据仍返回 `combination_verified: false`。
4. 首次组合离线构建缺少 lint-gradle 31.7.3，失败收据保留，构建锁释放；没有把下载缺项判为 API 回归。
5. 下载缺项后，同时核验两个候选，执行 `assembleDebug` 和 `testDebugUnitTest` 成功；44 个任务中 12 个执行、32 个复用缓存，耗时约 79 秒。
6. 运行历史保存在 `F:\ar\agents\combination-review\7668d51efcd67557882e\integration-receipts`，每次运行有 `inputs.json`、`verify.gradle`、`receipt.json`，实际校验完成时还有 `evidence.json`。
7. 故意把版本属性改回旧 `0.1.0-dev.c8e2e563065c`：真实 Gradle 的 `verifyIntegrationCandidates` 失败，状态查询返回 false。失败运行编号 `d368e9e99f14312793ef7901c357f2dddf2982e29480341ef8a0fc7d2cb2cf47`。
8. 恢复指定版本后离线验证通过，约 25.5 秒；同时选中 2 个候选、归档 54 个制品、测试 2 项、失败 0。最终运行编号 `3452342c136c66ce298829c88ebb02e746e0339fd2b43dad2def3eac9c46ac66`，日志 `gradle-86764-20260926T173518Z`（日志时间为 UTC）。查询退出码 0，`combination_verified: true`，`release_approved: false`。
9. 完整 Rust 回归 42 项全部通过，包含本轮 7 项组合回归和另一任务新增的缓存导入回归；格式检查和 release 编译通过。本轮使用 `target-integration` 独立构建目录，未覆盖另一任务正在使用的二进制。

同目录另一任务同时修改缓存导入代码时出现过短暂缺失和重复定义编译错误。经任务协调将组合逻辑移到独立 `integration.rs` 后解决；不是增量编译未发现问题。缓存导入由另一任务负责，本轮未将该任务的成果当作组合验证实现。

## Desktop 生命周期演练

在 `F:\dt-base` 测试克隆中，Worktrunk 通过 `.config/wt.toml` 创建了 Android、Compose 和集成 Worktree；每个 `pre-start` 都成功创建了独立 runtime、Gradle 用户目录和 Maven 目录。Android 的 AAR/core 编译和 22 项 core 测试通过，Compose AAR 打包和 2 项测试通过；Compose 首轮离线测试因预热快照未包含 Robolectric 等运行时依赖失败，随后只在自己的 Maven 目录补齐依赖后通过。

同一设备重复租约被拒绝，第二个 Agent 可租用另一台设备；ADB `get-state` 成功，租约已释放。同一 Worktree 的第三个并行构建被构建锁拒绝，另一 Worktree 继续构建。

注入故意失败的 core 测试后，`wt merge` 返回非零且目标提交不变；移除探针并重新测试后合并成功。提供方改名 `setStateListener` 为 `observeState` 后，候选事件识别一个删除和一个新增；消费者保留旧调用时编译失败，迁移后 Compose 两项测试通过。

演练中发现并修复两个入口问题：PowerShell 环境输出多一层引号，以及包装脚本转发 Gradle 参数后 CLI 误报多余参数。修复后的二进制已重新安装，SHA-256 与 `target-desktop` 构建一致。

测试服务已停止，Worktree 和设备租约已回收。原始 `superAppAndroid-validation`、`F:\sa4`、`F:\sa5`、`F:\sa6` 未被删除；演练克隆 `F:\dt-base` 及 `.desktop-trial` 证据仍保留。

## 限制

- 这是一个真实 AAR 加一个真实 JAR 的组合实验，不是十几个独立 AAR 仓库的规模验收。
- 当前指定的是 Compose 库构建及 JVM 测试，未执行 App release 和设备测试。查询始终返回 `release_approved: false`。
- 第三方制品解析结果已经归档，但尚未建立完整依赖锁与 JDK/SDK/环境输入指纹，后续构建仍应重新验证。
- 来源相同但命名不同的重复类仍需 App 的重复类检查发现；混用检查只覆盖相同 Maven 模块身份。
- 消费者范围依赖事件登记，尚无全组织实际依赖图。实验事件中明确缩小的范围不能用来为正式发布放行。
- 同一账户可改写本机状态和构建脚本；本机证据用于防误用和审计，不能替代可信 CI 的权限隔离。
- 整体失败归因、自动通知、跨仓升级 PR 和稳定组合晋级/回滚尚未完成。本轮没有宣称这些目标已交付。

## 重放

先启动本机回环 Maven 服务并验证实验事件，随后在 `F:\sa5` 执行：

```powershell
$env:AGENT_RUNTIME_ROOT = 'F:\ar'
$env:AGENT_ID = 'combination-review'
$agentctl = 'F:\code\worktree\agent-runtime\target-integration\release\agentctl.exe'
$manifest = 'F:\code\worktree\agent-runtime\examples\real-combination\manifest.json'
& $agentctl run-integration --manifest $manifest -- '-PHEAVY_DRAG_ANDROID_VERSION=0.1.0-dev.c8e2e563065c.metadata1' --offline
& $agentctl integration-status --manifest $manifest --format json
```

需要与初次测试相同的 JDK/SDK 环境。初次缓存未齐不能使用 `--offline`。回环服务仅用于本机实验。
