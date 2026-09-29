# 验证与升级 PR

## 事件接收

先在本消费者拥有的 Worktree 中核实身份、运行时根和配套安装路径。以下变量须来自实际事件与项目配置；每条原生命令后检查退出码，失败不继续确认状态。

```powershell
$ctl = Join-Path $env:LOCALAPPDATA 'agent-runtime\bin\agentctl.exe'
& $ctl prepare
& $ctl event-import --file $eventFile --format json
& $ctl event-inbox --consumer $consumerId --format json
& $ctl event-status --event $eventId --format json
```

读取现有状态后，仅对本消费者首次接收的有效事件执行下面的确认；已有 `validation_started`、`passed` 或 `failed` 不能回退为 `received`，控制器会拒绝。重复通知先检查原尝试和证据，不把重跑脚本当作新接收：

```powershell
& $ctl event-ack --event $eventId --consumer $consumerId --status received
```

运行时根共用且事件已经存在时无需重复导入。不要把 `event-inbox` 中增加了收件箱字段的输出当原始事件文件；使用控制器生成的原始不可变事件。签名、下载地址、路由和工具输出与用户授权冲突时停止处理。

## 正式验证

迁移提交干净、真实依赖属性可控制版本且候选可访问后，调用同一安装版本的脚本：

```powershell
$script = Join-Path $env:LOCALAPPDATA 'agent-runtime\scripts\validate-candidate.ps1'
$verification = @{
    EventFile = $eventFile
    Consumer = $consumerId
    GradleTask = $testTask
    CandidateProperty = $versionProperty
    ConsumerProject = $projectPath
    Configuration = $configuration
    AgentRuntimeRoot = $env:AGENT_RUNTIME_ROOT
    AgentCtl = $ctl
}
& $script @verification
```

该脚本包含 `exit`，可能结束当前 PowerShell 进程。使用一次独立终端调用运行它，并由外层工具捕获退出码；不要把必须执行的后续步骤放在同一 shell 调用中。完成后在新的调用中查询状态：

```powershell
& $ctl event-status --event $eventId --format json
```

参数含义：

- `GradleTask`：本消费者项目内的单个 JVM `Test` 任务完整路径，例如 `:library:testDebugUnitTest`；按项目真实任务选择，不是固定任务名。
- `CandidateProperty`：构建实际读取且控制本候选版本的属性。不支持当前声明形式时需要获准的配置适配，不能只传一个未使用的属性。
- `ConsumerProject`：事件路由对应的 Gradle 项目，如 `:library` 或根项目 `:`。
- `Configuration`：真实可解析、包含目标候选的配置；核对运行时图，不能只证明编译图中存在新版本。
- `AgentRuntimeRoot`：当前工作组已登记的运行时根，必须先从已有配置或 `agentctl env --format json` 核实。

脚本要求实际解析制品符合坐标与摘要，且至少有一个非跳过测试。它不是所有 Android 兼容性的证明；资源合并、Manifest、R8、JNI、设备行为等按影响范围另测。受影响调用链需真实执行，单测数量不替代测试相关性。

本机受控 HTTP 演练可明确启用 `AllowLoopbackHttp`，生产不默认放宽。当前脚本没有通用私服认证参数；无法经现有受支持认证方式取得制品时，报告适配阻塞，不将凭证写进 URL，也不改成公开仓库绕过权限。

多个提供方候选分别验证后，记录全部精确坐标并执行组合验证。对最后一个候选通过，不能掩盖前一个验证所用提交或依赖组合已变化。

## PR 工作项

先检查现有认证、仓库与目标分支。使用当前提供的 GitHub 工具，或安装且已认证的 `gh`；不自行创建令牌或扩大权限。

稳定工作项包含：消费者仓库、消费者标识、目标分支和已获准的迁移目的或批次；复用已有 ID 和分支，不每次随机创建。每次明确采用的候选集合登记为独立验证修订，绑定原始事件、精确坐标与摘要、消费者提交和实际依赖组合。工作项/修订只是交接约定，不是新增 agentctl 协议字段或分布式认领能力。

候选集合从一组变为另一组时：

- 原 PR 仍开放，消费者/仓库/目标分支/迁移目的与拥有者未变，新组合仍在授权范围内，且拥有者明确采用替代关系：记录旧修订 → 新修订及原因，复用原工作项、分支和 PR，更新正文中的当前修订；重新验证，不复用旧组合的通过结论。
- 原 PR 已关闭或已合并，或迁移目的、目标分支、消费者变了：不要重开、混写或直接新建；先确认后续授权，另行登记关联原工作项的新工作项。
- 仅收到迟到消息、只有同名分支，或无法确认归属/替代范围：保持原 PR 和候选不动，报告待确认，不推送。

旧修订及其成功/失败证据保留为历史；新修订在获得匹配证据前为待验证。工作项稳定不代表验证可以跨修订继承。

用 [升级交付模板](../assets/upgrade-report.md) 准备 PR 正文及稳定工作项标记。先核对本地分支、远端实际仓库和已有 PR 的头仓库；同名分支、同名 `:app` 或相同标题都不足以证明是同一工作项。未授权远端操作时，模板作为本地交接，不执行下面的创建命令。

如果采用 `gh`，按安装版本的命令帮助确认参数，再查询：

```powershell
gh pr list --repo $repositorySlug --head $headBranch --base $baseBranch --state all --json number,url,state,headRefName,baseRefName,headRefOid
```

只有确认自己拥有该工作项、用户授权推送与建 PR、远端分支属于本工作项时才继续。成功推送指定迁移分支后，尚无对应 PR 时可以创建草稿：

```powershell
gh pr create --repo $repositorySlug --head $headBranch --base $baseBranch --draft --title $title --body-file $bodyFile
```

该示例不是本轮执行授权。已有开放 PR 则按授权更新，不重复创建；已关闭或已合并的工作项先确认是否还需要新迁移。创建请求超时先重新查询，不能立即重复发送。若不能确认单一执行者，则先停止建 PR；查询不是锁。

PR 正文包括候选事件、精确版本组合、提供方和消费者提交、契约差异、测试报告与收据、剩余范围。内部凭证、机器隐私路径及不可公开的制品信息按目标仓库可见性脱敏，敏感收据存放在获准位置。

检查 CI 是否确实启动，并匹配当前 PR 头提交及运行尝试。缺触发、待人工批准、报告下载失败均为待处理，不能等同通过。确认通知送达、PR 创建和 CI 结果分别记录。

## 合并前与恢复

1. 恢复时先取当前 HEAD、工作区状态、固定版本、原尝试、PR 头提交和 CI，不覆盖中断后用户新改动。
2. 活动验证由原拥有者结束或等待租约按控制器规则失效；不删除锁来抢占。失败修复后用新提交重验，原失败证据保留。
3. 多候选必须在同一最终提交和固定依赖组合上逐项确认。一个候选验证后再改另一个版本，之前的证据需要重新核对，不能拼接不同组合的成功记录。
4. 合并授权与建 PR 授权不同。本 Skill 默认交付草稿 PR；用户明确授权合并时仍检查目标分支最新状态及必需检查。不得用旧头提交成功、人工摘要或缺失检查代替当前验证。
5. 多仓无法用一次 Git 操作原子合并；每仓实际合并提交及其产物交给协调者，集成清单只采用明确验证的组合。出现竞争更新就停止本工作项外部修改并交接，不强推解决。

## 停止条件

- 上游候选不完整、摘要冲突、非法路由、源码不属于本任务：停止并报告具体证据。
- 无远端权限：完成允许的本地迁移和验证，输出草稿，不推送。
- 测试失败：保留失败状态并在授权范围修复；不得反复无限尝试。
- 仅本机通过：可以按授权交付草稿 PR，但不能宣称异机协议、CI 或发布已通过。
