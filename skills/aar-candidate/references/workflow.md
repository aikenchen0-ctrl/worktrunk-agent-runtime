# 候选命令与交接

## 环境与命令

以下片段是供 Agent 在完成参数核对后执行的命令，不是可直接批量粘贴的脚本。变量必须来自当前项目；不要把示例值当作真实仓库配置。每次原生命令后立即检查 `$LASTEXITCODE`，失败时停止本阶段。所有路径参数以字符串传递，不拼接可执行表达式。

```powershell
$ctl = Join-Path $env:LOCALAPPDATA 'agent-runtime\bin\agentctl.exe'
& $ctl --help
& $ctl --version
& $ctl prepare
& $ctl doctor --format json
& $ctl env --format json
```

已安装路径可被项目明确配置的其他路径替代；不要下载来源不明的二进制。保留既有 `AGENT_ID`、`AGENT_RUNTIME_ROOT`，主 Worktree 默认不加 `--allow-primary`。

提交已获准的修改且工作区干净后生成候选：

```powershell
$candidate = & $ctl candidate-version --base $baseVersion
if ($LASTEXITCODE -ne 0) { throw '生成候选版本失败' }
$candidate = ([string]$candidate).Trim()
& $ctl contract-diff --module $moduleDirectory --baseline $baselinePath --format json
```

`$moduleDirectory` 是本模块源码目录，不是消费者 Gradle 项目路径。基线属于最近一次已发布版本且范围相同，不能在已改动源码上重新生成“旧基线”。首次发布没有基线时显式标注；当前事件省略基线时可能默认给出 `breaking: false`，这不构成兼容性判断。

`contract-diff --enforce` 可用于策略检查，破坏性变更会返回非零。既定策略允许破坏性候选时仍记录差异和迁移计划，稳定升级门槛保持不变；不删除失败日志或偷偷改策略。

## 发布任务

先检查实际 Gradle publication 和版本参数是否生效，再执行真实任务：

```powershell
$publishArguments = @($publishTask, "-P$versionProperty=$candidate")
& $ctl run-gradle -- @publishArguments
if ($LASTEXITCODE -ne 0) { throw '候选发布失败，先核对远端是否部分写入' }
```

`$publishTask` 必须是项目已经配置、指向获准候选仓库的精确任务。若版本不是 Gradle property 驱动，不照搬这段；按实际配置传值并核对最终坐标。不要调用无关 publication 的聚合发布任务。

在候选仓库不可覆盖规则生效且制品完整可读后，比对 AAR/JAR、POM、模块元数据及必要依赖。事件主制品摘要不能替代元数据检查。认证通过现有凭证机制，不在 URL、日志或接口文档中写令牌。

## 事件创建

先查询已有事件，并核对提供方提交、坐标、摘要和消费者集合；当前 `contract-event` 每次会生成新标识，不自带业务去重。由一个指定的候选发布者创建事件，重试不能制造重复工作项。

```powershell
& $ctl event-status --format json
$eventArguments = @(
    'contract-event', '--provider', $providerId,
    '--module', $moduleDirectory, '--base', $baseVersion,
    '--baseline', $baselinePath,
    '--artifact-url', $artifactUrl,
    '--artifact-sha256', $artifactSha256,
    '--artifact-coordinate', $coordinate,
    '--consumer-targets', $consumerTargetsPath,
    '--format', 'json'
)
$eventJson = & $ctl @eventArguments
if ($LASTEXITCODE -ne 0) { throw '创建候选事件失败' }
$event = $eventJson | ConvertFrom-Json
```

没有基线或显式跨仓路由时，仅按实际规则省略对应成对参数。事件使用创建时的当前提交；发布与创建事件之间不要继续修改或提交业务代码。动态交接信息放在外部运行时，不为写入候选 SHA 再改动源码提交。

事件文件通常位于有效运行时根的 `events/<event_id>.json`；先通过工具环境输出核实根路径并检查文件，不假定使用默认目录。禁止修改事件中的路径、版本或摘要。

路由 JSON 的实际格式为“消费者唯一标识 → 本机仓库绝对路径与 Gradle 项目”。例如字段结构：

```json
{
  "buyer-app": {
    "repository": "F:/repos/buyer",
    "project": ":app"
  }
}
```

这是格式示例，不代表该仓库或消费者存在；执行时使用已经核实的真实映射。不同仓库不能共享同一消费者标识。当前不支持把 Windows 本机路径直接当作另一台 CI 机器的仓库身份。

## 交接清单

交接信息与版本化迁移说明一起提供；它不是另一种可直接导入的 agentctl 事件协议。

| 信息 | 必须说明 |
| --- | --- |
| 身份 | 提供方仓库、模块、完整提交、变体、工具链 |
| 契约 | 基线版本、旧签名、新签名、行为和依赖变化、破坏性判断依据 |
| 迁移 | 调用示例、适配步骤、哪些消费者需改代码 |
| 候选 | 固定坐标、下载入口、制品摘要、元数据检查结果、候选就绪证据 |
| 路由 | 受影响消费者、直接或传递路径、尚未确认的依赖范围 |
| 验证 | 已跑任务、测试数量、失败项、收据或报告位置 |
| 事件 | 工具生成的事件 ID、事件文件、重复工作项处理情况 |

依赖草稿可以提前沟通，但必须标明没有可消费制品。不得把草稿说明发成“候选验证已就绪”。

使用 [候选交接模板](../assets/candidate-handoff.md) 保存同一工作项的阶段与证据。已发布源码里的迁移文档不要求包含事后事件 ID；通过外部交接把完整提交、文档路径、候选和事件关联起来，避免为填入摘要而再次改变源码提交。

## 发布中断的继续位置

| 核实结果 | 下一动作 | 不能做的事 |
| --- | --- | --- |
| 只有草稿，无可消费制品 | 继续实现，或交接明确标记的桩/适配方案 | 生成就绪通知或要求正式 passed |
| 远端无该坐标，仍在原发布授权内 | 按原固定提交和输入执行一次发布 | 顺便发布其他模块 |
| 远端完整且所有必要摘要与来源匹配 | 复用制品，查已有事件；只补缺失交接 | 因通知失败重新上传 |
| 上传结果未知或仅部分制品存在 | 查询服务端状态；只使用服务支持的受控恢复 | 用新版本掩盖残缺发布或覆盖同坐标 |
| 同坐标但字节或来源不一致 | 停止并核对源码、工具链和依赖输入 | 将短 SHA 当成内容相同的证明 |
| 事件已经生成，消费者尚未处理 | 保留原事件，交给协调者路由 | 改事件路径绕过身份检查或重复造事件 |

远端未配置不可覆盖或受支持认证时，先完成本地候选准备并列出解除阻塞的配置，不把这些缺口“用 Skill 解决”。
