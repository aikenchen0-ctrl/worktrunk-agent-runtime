# 并行 API 演化实测：2026-09-27

> 文档中的用户目录已匿名化；示例绝对路径须按实际机器替换。

## 范围与结论

真实测试来源为 `superAppAndroid` 的 `backSelfLanuchOKcnnOK` 分支，原始检出提交为 `536735372568ddb1d2050b03e2b2744c4c2ad9cb`。演练在独立测试克隆及其 Worktree 中进行。收尾检查时，原始检出 `F:\code\worktree\superAppAndroid-validation` 有 `AGENTS.md` 和 `docs/agent-cache-setup.md` 两处未提交改动，本轮未覆盖；下列四个演练 Worktree 均保持干净。

本轮已执行三个提供方 Agent 的接口修改、真实 Kotlin/Android 构建、消费者迁移和二进制组合验证。三个提供方包含两个 AAR 模块和一个 JVM JAR 模块；不能表述为十几个独立 AAR 仓库，也没有完成十几个 Codex Desktop 窗口的自动调度测试。

组合验证已通过，但两个候选仍各有 app、accessibility 两个消费者未完成验证，共四个待验证关系。因此 `combination_verified=true`、`ready_for_integration=false`、`release_approved=false`。

## 实际发生的问题

| 变更 | 观察结果 | 处理 |
| --- | --- | --- |
| Core 删除 `handlePointerEvent`，改为 `dispatchPointerEvent` | 未迁移的 Android 调用编译失败 | Android 修改调用并重新发布候选 |
| Android 删除 `setStateListener`，改为 `observeState` | 未迁移的 Compose 调用编译失败 | Compose 修改调用并使用固定 Maven 候选 |
| Overlay 的 `edgeOverlayOutlineColorArgb` 增加必填参数 | accessibility 缺少参数，编译失败 | 保留失败证据；未假报全部消费者完成 |
| 旧 Android AAR 与新 Core JAR 混合 | 编译和打包可通过，实际调用发生 `NoSuchMethodError` | 增加调用真实跨制品链路的 Robolectric 测试 |
| 候选版本手工截取提交 SHA | 事件版本与制品版本不一致 | 坐标版本严格一致；Agent 规则要求直接使用版本生成命令输出 |
| 多个仓库都有 `:app` | 仅按模块名不足以确认消费者身份 | 事件绑定本机仓库身份、Gradle 项目和独立消费者标识 |
| `run-integration --format json` 混入构建日志 | Agent 无法直接解析完整标准输出 | 日志与诊断转标准错误，原始双流日志分别保存 |
| ADB 查询没有等待上限 | 诊断入口可能一直等待查询或输出结束 | 查询增加超时，输出使用临时文件；只终止本次查询客户端 |
| 无线 ADB 序列号包含空格 | 按空白分词会把序列号后半段误判为设备状态 | 优先按制表符分隔，补充含空格序列号的回归用例 |

## 真实构建证据

- Core Worktree：`F:\api-heavy-core`，提交 `2f0685fc8847575f8cacff174ea3eef77d0c441a`。
- Android Worktree：`F:\api-android`，迁移后提交 `a78a6a85296b1ca28b7abe8a6fa2041f75ac67b7`。
- Overlay Worktree：`F:\api-overlay`，提交 `ac75ed871d421325aae06fb65167ec5c83facff3`。
- 组合 Worktree：`F:\api-combination`，提交 `663b22810926b2e28a553544e64f7d3d8398676e`。

三个提供方的具体记录分别位于其 `docs/api-heavy-core-trial.md`、`docs/api-android-trial.md`、`docs/api-overlay-trial.md`。

当前组合候选：

| 制品 | 精确版本 | SHA-256 |
| --- | --- | --- |
| `com.paifa.univerge:univerge-heavy-drag-core` | `0.4.0-dev.2f0685fc8847` | `0f5e129f106522ec823dadc73d9ae7052f03fed48eb4d03ea6c6b9e81da19760` |
| `com.paifa.univerge:univerge-heavy-drag-android` | `0.4.0-dev.a78a6a85296b` | `653f2017de6a41e77768d96e9941a70574f1f0d52a687b37dd5e1edf1f2326a6` |

正式消费者验证由 `validate-candidate.ps1` 产生：

| 消费者与候选 | 实际测试 | 结果 |
| --- | --- | --- |
| Android 消费新 Core | 11 项，0 失败，51.806 秒 | passed |
| Compose 消费新 Android | 3 项，0 失败，57.009 秒 | passed |
| Compose 消费新 Core | 3 项，0 失败，39.411 秒 | passed |

前两项在不同 Worktree 同时开始构建，使用各自的 Gradle 用户目录和构建锁。Compose 的两次正式验证按顺序执行。

Core 事件为 `20260927T081723-79f6135d2ca0`，Android 事件为 `20260927T083933-472e0e0f1102`。确认文件保存在 `F:\ar\events\<事件ID>\acks`，没有手工写入 passed。

旧组合的失败收据：

```text
F:\ar\agents\api-combination-agent\646c097e1dd674b0a89f\integration-receipts\361f5436ebad2b1e6b10f151dce7621fff02d93764fdeb5a80db393018cafd43\receipt.json
```

该收据记录 3 项测试、1 项失败。日志 `gradle-63808-20260927T084342Z.stdout.log` 含真实 `NoSuchMethodError`。

修复后的最后一次真实组合收据：

```text
F:\ar\agents\api-combination-agent\646c097e1dd674b0a89f\integration-receipts\1a928eef78f91e57d99b5a2283da6ecac872342488e10b85b9e6ce0359d1b937\receipt.json
```

实际执行 `assembleDebug` 和 `testDebugUnitTest`，3 项测试、0 失败，26.303 秒。输出文件 `logs/integration-json-output.json` 已用完整 JSON 解析验证，诊断另存 `logs/integration-json-diagnostics.log`。

默认入口已实测拒绝四个待验证关系。只有显式 `--allow-pending-consumers` 才执行探索组合；该参数不会替消费者确认迁移，也不会批准发布。

## 控制器修复与自动化测试

- 跨仓路由绑定仓库、Gradle 项目和消费者标识；同名 `:app` 不再混用确认。
- YAML 声明而未被本仓依赖图发现的消费者必须显式配置路由。
- 版本化事件缺路由拒绝导入，不回退为旧格式；Windows 大小写冲突拒绝。
- 支持根项目和显式嵌套项目；拒绝用子项目测试冒充目标项目测试。
- 状态显示与集成入口统一重新检查完整确认收据。
- 非标准 Git 布局暂时明确拒绝，避免误判仓库归属。
- 旧未知归属的活动验证不抢占；需原 Agent 结束或租约到期。
- JSON 结果与进程诊断分流；退出码为零但缺测试证据仍拒绝。
- ADB 查询默认 10 秒超时；超时不关闭共享 Server。查询输出使用临时文件，避免后台 Server 持有管道时等待不到输出结束。
- ADB 设备解析保留序列号中的空格，并识别完整的 `no permissions` 状态。

已通过的完整回归命令：

```powershell
cargo test --locked --target-dir target-parallel-api -- --test-threads=4
```

日志 `.desktop-trial/final-regression.log`：58 项单元测试、7 项真实 Git/CLI 路由测试、3 项模拟进程输出协议测试，合计 68 项通过、0 失败。输出协议模拟不等于 Gradle 或 Android 实测。随后 `cargo build --release --locked --target-dir target-parallel-api` 成功。

## 设备诊断、安装与回收

最终发布构建执行 `doctor --format json`，2.952 秒返回，退出码 0，14 项检查通过。证据为 `.desktop-trial/bounded-doctor-final.json`。本次实际识别 USB 设备 `885bed6d`、`ac796d4e`，状态均为 `device`；没有执行安装和真机行为验收。最终查询未出现无线设备，含空格无线序列号修复由回归用例验证，不能据此宣称最终无线连接已实测。

已将同一批次二进制和三个验证脚本安装到 `C:\Users\developer\AppData\Local\agent-runtime`，安装时间为 2026-09-27 17:56:10（UTC+8）。安装后逐项核对 SHA-256，并确认入口可以启动。程序摘要：

```text
41015B930660F201F1036C3A57A63CB4865B0BF049CA5F8608C8DA84FCB99212
```

安装记录为 `.desktop-trial/installation.json`，旧版备份为 `C:\Users\developer\AppData\Local\agent-runtime\backups\parallel-api-20260927-41015B930660`。安装后的 `integration-status` 再次读取实际收据，仍报告组合通过、四个关系待验证、发布未批准，退出码 1 符合当前未完成状态。

2026-09-27 17:59:54（UTC+8）核对 PID、启动时间与命令行后，停止本轮启动的候选 Maven HTTP 服务，并清理 `api-combination-agent` 的运行状态。清理后状态为 `cleaned`，端口记录为空，源码、Gradle/Maven 缓存、日志及成功和失败收据均保留。未删除演练 Worktree，未停止共享 ADB Server。该本轮临时仓库端口不再监听；后续联调需要重新登记并启动候选仓库服务，或使用长期制品仓库。

## 尚未完成

1. 十几个独立 AAR 仓库、十几个真实 Codex Desktop 任务同时变更 API 的全量演练。
2. app/accessibility 对这组候选的全部迁移与验证，以及 Overlay 候选的完整消费者闭环。
3. 多候选 App release、真实设备行为、签名发布和回滚验证。
4. 跨机器稳定仓库身份、可信事件分发和 CI 收据导入。
5. GitHub 升级 PR 自动创建、自动触发消费者验证、闲置 Desktop 任务可靠唤醒。
6. 全组织实际依赖图、完整 ABI/资源/Manifest/行为兼容性检查。

本地事件收件箱、私有缓存和测试收据已形成部分闭环；以上缺项意味着整个原始需求尚未完成，不能给出“全部可用”的验收结论。
