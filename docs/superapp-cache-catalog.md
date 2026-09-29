# SuperApp 依赖预下载目录与验证范围

> 文档中的用户目录已匿名化；示例绝对路径须按实际机器替换。

本目录针对提交 `536735372568ddb1d2050b03e2b2744c4c2ad9cb`。只保存实际构建验证过的版本组合；Gradle、AGP、Kotlin、SDK、NDK、CMake 或项目提交变化后生成新快照，不覆盖旧版本。

## 推荐快照

以 `F:\code\worktree\agent-runtime` 为根，当前推荐 `.cache-snapshots/superapp-complete-v5`。有效载荷为 24,481 个文件、2,228,081,250 字节，约 2.08 GiB，内部分类如下：

| 分类 | 内容 |
| --- | --- |
| `gradle-distribution` | Gradle 8.10.2 发行包 |
| `gradle-dependencies` | 主工程、Blinkvoice、Touch 的插件、业务依赖和传递依赖 |
| `maven-test-runtime` | Robolectric Android 15、14、8、6、4.4 framework 运行时 |
| `configuration-reports` | 三个构建根本轮配置解析结果 |

本快照从同一份受构建锁保护的种子缓存产生，没有合并不同种子的 Gradle 元数据。后续生成脚本强制使用空种子缓存，并只收录本轮三个配置报告。共享快照保持不可变；`install-cache` 将其复制到每个 Worktree 私有且可写的 `GRADLE_USER_HOME` 和 `MAVEN_REPO_LOCAL`。

## 已验证构建矩阵

| 构建根 | 固定版本与入口 | 全新私有缓存离线结果 |
| --- | --- | --- |
| 主工程 `.` | Wrapper Gradle 8.10.2、Kotlin 2.0.21 | 全新私有缓存执行 `:adbcore:assembleDebug`、`:app:bundleRelease`、`:univerge-overlay:testDebugUnitTest`，234 个任务全部重新执行成功；覆盖四种 ABI Native、Release Bundle 和 Android 6 Robolectric |
| `blinkvoice-visual-sdk` | 快照内固定 Gradle 8.10.2，使用 `scripts/warm-mirrors.gradle` | Debug、Release、50 个单元测试，56 个任务全部重跑成功 |
| `univerge-touch` | AGP 8.7.3、compileSdk 34；快照内固定 Gradle 8.10.2 | Debug、Release 和发布元数据已预热；单元测试仍被现有源码错误阻塞 |

主工程配置扫描解析成功 344 项、跳过 55 项、失败 0 项；Blinkvoice 为 40/6/0，Touch 为 29/6/0。跳过项是 Gradle 内部项目变体歧义，未被描述为已经解析。App、adbcore、AndroidTest、Benchmark、AAR 发布元数据和三套主工程 Robolectric 任务均已进入预热矩阵。

Touch 仓库自带 Gradle 8.7 Wrapper，与 AGP 8.7.3 不兼容。当前必须使用快照内固定 Gradle 8.10.2；缓存不能修复仓库 Wrapper 声明。Touch 单元测试仍受现有 Java 命名空间迁移和类型可见性错误阻塞，依赖已经预热，不能把该失败归因于下载缺失。

## 新 Worktree 使用

在尚未构建的新 Worktree 根目录执行：

```powershell
$runtime = 'F:\code\worktree\agent-runtime'
$env:AGENT_RUNTIME_ROOT = 'F:\ar'
$env:JAVA_HOME = "$runtime\.tools\msjdk17\jdk-17.0.20.1+1"
$env:ANDROID_HOME = 'C:\Users\developer\AppData\Local\Android\Sdk'
$ctl = "$runtime\target\release\agentctl.exe"

& $ctl prepare
if ($LASTEXITCODE -ne 0) { throw '准备运行时失败' }
& $ctl install-cache --snapshot "$runtime\.cache-snapshots\superapp-complete-v5"
if ($LASTEXITCODE -ne 0) { throw '导入依赖失败' }
& $ctl run-gradle -- --offline --no-daemon '-Pkotlin.compiler.execution.strategy=in-process' :app:assembleDebug
if ($LASTEXITCODE -ne 0) { throw '离线构建失败' }
```

`local.properties` 继续放在各 Worktree 内且不提交：

```properties
sdk.dir=C\:\\Users\\Paifa\\AppData\\Local\\Android\\Sdk
cmake.dir=F\:\\code\\worktree\\agent-runtime\\.tools\\cmake-4.4.3\\cmake-4.4.3-windows-x86_64
```

Blinkvoice 和 Touch 没有可直接复用的兼容 Wrapper。先读取 `agentctl env --format json` 得到私有 `GRADLE_USER_HOME` 与 `MAVEN_REPO_LOCAL`，再从 `GRADLE_USER_HOME\wrapper\dists\gradle-8.10.2-all` 选择 `gradle.bat`。执行前通过 `agentctl lock-build` 获取同一 Worktree 构建锁，并加入 `-I scripts/warm-mirrors.gradle`；测试再加入 `-I scripts/runtime-test-isolation.gradle`。这两个入口后续应由外挂服务封装为固定 Profile，减少 Agent 手工拼接命令。

## 机器级工具链

Gradle 快照不包含 JDK、Android SDK、Build Tools、NDK 和 CMake。当前工具链清单在 `.cache-snapshots/toolchain-inventory.json`，要求 JDK 17.0.20.1、Android 平台 34/36、Build Tools 34.0.0/36.0.0、NDK 28.2.13676358、SDK CMake 3.31.6、独立 CMake 4.4.3、Ninja 1.12.1、Platform Tools 37.0.1 和 Command-line Tools 20.0。模拟器 36.1.9、Android 36.1 Play Store x86_64 系统映像及 sources 36 已列为可选机器资源。真机和 ADB 设备租约另行管理。

## 剩余缺口

- `install-cache` 当前校验主工程 Wrapper 摘要并拒绝覆盖非空缓存，但还没有逐文件哈希、事务安装和安装收据。
- 当前需要手工填写快照绝对路径，尚未提供按仓库、提交和构建根自动选择的 `cache list/resolve`。
- Touch 单元测试、真机测试和 Benchmark 真机执行尚未通过。
- 当前快照只覆盖提交 `536735372568ddb1d2050b03e2b2744c4c2ad9cb` 实际声明和执行过的版本；未来新增或升级依赖时必须生成新快照，不能预下载尚未出现的坐标。
- 缓存就绪只证明声明任务无需临时下载，不代表跨 AAR API 迁移、组合兼容性或设备验收已通过。
