# 依赖预下载规则

本仓库中的 Codex Desktop Agent 必须在独立 Git Worktree 中工作。首次执行任何 Gradle 任务前，先阅读 `agent-runtime/docs/codex-agent-cache-guide.md`，安装当前项目登记的不可变依赖快照。

## 启动顺序

1. 使用任务分配的稳定唯一 `AGENT_ID`，并与所有其他 Agent 共享同一个 `AGENT_RUNTIME_ROOT`。
2. 核对当前目录是预期 Worktree 根目录，不能在主 Worktree 或其他 Agent 目录执行。
3. 设置 JDK 17、Android SDK 和短运行时根目录。
4. 调用 `scripts/install-superapp-cache.ps1` 导入快照，并使用当前模块的真实任务做离线验证。
5. 依赖快照安装成功后，所有 Gradle 操作继续通过 `agentctl run-gradle` 执行。

## 禁止事项

- 禁止直接使用全局 `~/.gradle` 或 `~/.m2` 作为多个 Agent 的共享可写缓存。
- 禁止运行 `publishToMavenLocal` 向用户级 Maven 仓库发布跨仓 AAR。
- 禁止在业务仓库加入 `mavenLocal()`、动态版本或 `SNAPSHOT`。
- 禁止从另一个 Worktree 复制正在使用的 Gradle、Maven、`.gradle` 或 `build` 目录。
- 禁止在私有缓存非空时覆盖安装快照。
- 禁止把本机路径、设备序列号、`local.properties` 或缓存目录提交到仓库。
- 禁止普通模块 Agent 使用 `-AllowSourceDrift` 绕过依赖描述指纹检查。

## 缓存缺失处理

离线构建缺少依赖时，先确认它确实是本次改动新增的坐标。可以在当前 Worktree 的私有缓存中联网验证以继续开发，同时记录坐标、版本、触发任务和工具链版本。由缓存维护任务使用新的空种子生成下一版不可变快照。不要修改现有快照，也不要让多个 Agent 分别维护共享缓存。

缓存安装成功只代表依赖可用。公开 API、资源、Manifest、序列化或 Native ABI 变化仍必须执行契约事件、消费者迁移和组合验证。
