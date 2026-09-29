# 验证与升级 PR

## 事件接收

先在本消费者拥有的 Worktree 中核实身份、运行时根和配套安装路径。以下变量须来自实际事件与项目配置；每条原生命令后检查退出码，失败不继续确认状态。

```powershell
$ctl = Join-Path $env:LOCALAPPDATA 'agent-runtime\bin\agentctl.exe'
& $ctl prepare
& $ctl event-import --file $eventFile --format json
& $ctl event-inbox --consumer $consumerId --format json
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

固定工作项包含：消费者仓库、消费者标识、目标分支、候选事件集合。已有工作项标识与分支名优先复用，不能每次运行创建随机分支。

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

## 停止条件

- 上游候选不完整、摘要冲突、非法路由、源码不属于本任务：停止并报告具体证据。
- 无远端权限：完成允许的本地迁移和验证，输出草稿，不推送。
- 测试失败：保留失败状态并在授权范围修复；不得反复无限尝试。
- 仅本机通过：可以按授权交付草稿 PR，但不能宣称异机协议、CI 或发布已通过。
