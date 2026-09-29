# 已知限制和下一步

## 当前限制

- 仓库身份目前支持标准独立克隆及其 Worktree；裸仓库、子模块和外置 Git 管理目录被显式拒绝，避免把共用父目录误判为同一仓库。

- `run-android-test` 和 `adb` 是受租约保护的设备操作入口；若其他程序直接调用系统 ADB，本服务无法从操作系统层强制拦截。`adb` 会拒绝外部设备选择器和 ADB Server 参数，避免绕过当前租约。
- PowerShell 设备租约具有到期时间，长时间空闲时需要运行 `agentctl heartbeat-device`；受控测试期间由服务自动续期。
- 设备列表查询默认最多等待 10 秒，可用 `AGENT_ADB_QUERY_TIMEOUT_SECONDS` 在 1 到 300 秒之间配置。超时只结束本次查询客户端，不停止共享 ADB Server；在线查询通过不等于设备安装或行为测试通过。
- `cleanup` 默认保留 Gradle 和 Maven 缓存；`--purge-cache` 会先通过该 Worktree 的 Wrapper 停止 Gradle 守护进程再删除缓存。
- 端口分配只登记端口并检查本机 TCP 端口占用，不会替 Agent 启动服务，也不会停止监听进程。
- Windows 可通过 `AGENT_CPU_PERCENT` 和 `AGENT_MEMORY_LIMIT_MB` 限制 Gradle 子进程树；Linux/macOS 目前只有 Gradle worker 和 JVM heap 限制。
- Worktrunk Hook 示例需复制到业务仓库主分支的 `.config/wt.toml`；首次运行需批准 Hook，并将 `agentctl.exe` 安装在 `%LOCALAPPDATA%\agent-runtime\bin`。
- 真实设备端到端验证依赖本机在线设备；自动化测试仍使用假的设备数据和 ADB 适配器，不把本机偶然连接的设备作为 CI 前提。
- Windows Native 构建可能因 Gradle 缓存路径超过 Ninja 的 260 字符限制而失败；可用全机统一的短 `AGENT_RUNTIME_ROOT`。不同 Agent 使用不同根目录会让设备租约与端口注册表失去互斥，不能混用。
- `contract-snapshot` 是轻量 Kotlin 文本扫描器，不等价于 Metalava、Kotlin ABI Validator 或运行时行为测试；复杂泛型、注解处理器生成 API、资源和二进制兼容性仍需 CI 中的正式工具验证。
- `contract-event`、`event-inbox` 和 `event-ack` 提供本机可重放通知，不直接连接 GitHub、Nexus 或 Codex 会话；跨机器通知和自动升级 PR 仍需 GitHub Actions、制品仓库和通知适配器消费 `events.jsonl`。
- `event-status` 只汇总单个候选事件的消费者确认；多个 AAR 同时升级后的组合兼容性必须由集成壳工程固定候选坐标后再次构建和测试。
- `run-integration` 绑定清单、事件、消费者确认和校验脚本摘要，在一次构建内检查整组候选制品及 JVM Test 执行结果。它不会自动生成集成工程或升级依赖；验证范围由清单指定，不代替 App 真机、签名发布和回滚。只记录选中第三方制品，尚无完整依赖锁与构建环境指纹。
- `--allow-pending-consumers` 仅允许联合迁移阶段先执行组合测试，不授予发布权限，也不产生消费者确认。探索通过与所有消费者完成迁移是两个独立状态；确认变化后必须重新验证组合。
- 消费者事件现支持显式路由，绑定本机仓库路径和 Gradle 项目，并为同名模块使用不同消费者标识。旧事件只能在提供方原仓库内验证；缺失或错误路由会拒绝确认。路径不是跨机器稳定仓库身份，尚不能直接用于全组织或异机 CI 通知。
- 多模块仓库必须按 AAR 模块目录分别生成契约基线；全仓快照不能直接与单模块快照比较，否则会把未参与变更的模块误判为删除。
- 构建收据绑定本机 Agent、Worktree、干净提交、本次验证编号、实际解析制品和测试执行数。跨机器不能直接复制收据确认通过，仍需可信 CI 证明导入机制。
- GitHub Actions 模板只下载已发布的 `agentctl.exe`；消费者仓库仍需把 `validate-candidate.ps1` 作为受审查的 CI 适配文件放入仓库，或者改写为等价脚本。外挂服务不会把业务仓库改造成 monorepo。
- 候选验证支持实际解析的直接和传递 Maven 依赖，拒绝同模块旧项目源码与新制品混用。当前证据适配器只支持 JVM Test；测试数量不能证明测试覆盖充足，设备测试和集成验收仍需独立完成。
- 验证编号和收据快照防止误用，不是防恶意伪造的安全边界。同一 Windows 账户仍可修改状态文件或构建脚本；可信发布需要独立 CI 权限和签名证明。
- 依赖通知图仍由源码扫描和手工消费者登记构成，尚未用全组织各仓库的实际解析图自动更新；版本目录、复合构建和传递跨仓关系可能漏报。
- 独立测试用户目录可能改变依赖用户主目录配置的测试行为；Robolectric 镜像必须显式配置，Gradle 的 `--offline` 不会自动禁止测试 JVM 自行访问网络。
- SuperApp v5 快照覆盖三套构建根的已声明依赖和五套 Robolectric 运行时，不包含机器级 JDK、Android SDK、NDK、CMake、模拟器或未来新增版本。配置扫描仍有 Gradle 内部项目变体歧义项被显式跳过；快照清单记录提交、工具链引用和离线复验结果，但 `install-cache` 尚未逐文件验签或生成事务安装收据。
- Gradle 命令行目录重定向已拒绝，普通构建移除继承的只读共享依赖缓存。构建脚本、初始化脚本和 JVM 环境选项仍属于可信代码，外挂不提供操作系统级文件写入沙箱。
- GitHub Actions 模板的 `event_file` 是 checkout 后本地路径；提供方事件的跨仓安全分发、可信签名和自动触发消费者 CI 仍未实现。`event-ack passed` 依赖本地构建收据，尚未提供跨机器不可伪造的 CI 证明。
- GitHub Actions 模板在临时 checkout 主目录运行，只适用于一次性 CI 验证；Codex 本地 Agent 必须使用 Worktrunk 创建的非主 Worktree，避免把 CI 的 `--allow-primary` 误用于持续开发。

## 后续扩展

- 为 Linux/macOS 增加进程组终止和 CPU/内存配额实现。
- 实现租约后台心跳服务和开机后孤儿资源清理。
- 扩大真实多 Worktree/多设备端到端测试；现有并发、失败锁释放和安装回归仍有模拟适配器，不等同实际 Android 组合验收。
- 新安装器和发布包已提供；签名发布、自动升级、安装中断恢复和工作区初始化仍待实现。
