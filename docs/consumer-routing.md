# 本机跨仓消费者路由

## 路由规则

消费者标识在一个事件内唯一，不等于 Gradle 项目名。两个独立仓库都含有 `:app` 时，应登记为 `buyer-app` 和 `seller-app`。

由提供方通过 `contract-event --consumer-targets <文件>` 读取以下 JSON 映射。仓库路径必须是本机已存在的合法 Git 仓库或 Worktree；控制器解析为 Git 所属仓库。下面是配置示例，不代表这些示例仓库已经建立。

```json
{
  "buyer-app": {
    "repository": "F:/repos/buyer",
    "project": ":app"
  },
  "seller-app": {
    "repository": "F:/repos/seller",
    "project": ":app"
  }
}
```

自动发现的本仓库消费者仍保留。显式映射可以纠正本仓库消费者的嵌套 Gradle 项目路径，但不能改指向另一仓库。`config/dependents.yml` 中未被本仓依赖图发现的消费者必须提供显式路由。新增跨仓消费者须使用独立标识，标识不能仅大小写不同。生成的 `affected_consumers` 与 `consumer_targets` 一一对应，路由作为不可变事件的一部分保存。

## 消费者操作

1. 在消费者 Worktree 执行 `agentctl prepare`。同机共用状态根目录时直接读取提供方事件；收到独立事件文件时再导入。
2. 执行 `agentctl event-inbox --consumer buyer-app --format json`。收件箱仅显示路由归属当前仓库的事件。
3. 迁移代码并提交后，用 `validate-candidate.ps1` 指定 `-Consumer buyer-app -ConsumerProject :app` 和该项目实际 JVM 测试任务。
4. 控制器同时检查仓库归属、Gradle 项目、提交、验证租约、实际解析制品和本次测试证据。

不能通过在另一仓库传入相同消费者名来领取验证或回填通过状态。状态汇总与集成入口都会重新验证收据，不把 JSON 中的 `status: passed` 单独视为证据。

## 迁移限制

- 旧事件未携带路由时，默认只允许提供方原仓库内的同名模块确认；不会猜测独立消费者仓库。
- 已有事件不能原地补写路由。创建新的候选事件，并重新验证相应消费者。
- 旧版未完成确认若无法证明仓库归属，显示 `needs_revalidation`；未到期租约仍保守阻止抢占，须由原 Agent 结束或等待到期。控制器不会为修复通知而覆盖未知的活动验证。
- 当前是本机路径身份；跨机器稳定仓库 ID、可信事件分发与 CI 收据导入尚未实现。
- 为避免仓库归属碰撞，暂不接受裸仓库、子模块或 `--separate-git-dir` 外置管理目录；普通独立克隆和其 Worktree 可用。
- 显式登记是人工声明，不证明已覆盖所有真实依赖；全组织实际依赖图仍待接入。
