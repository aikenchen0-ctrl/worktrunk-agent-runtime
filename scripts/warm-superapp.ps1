param(
    [Parameter(Mandatory = $true)]
    [string]$ProjectPath,

    [Parameter(Mandatory = $true)]
    [string]$SnapshotPath,

    [string]$AgentCtlPath = "agentctl",
    [string]$AgentId = "superapp-cache-seed"
)

$ErrorActionPreference = "Stop"

function Normalize-Path([string]$Value) {
    return [System.IO.Path]::GetFullPath($Value).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
}

function Get-DependencyDescriptorDigest([string]$RepositoryPath) {
    $trackedFiles = @(& git -C $RepositoryPath ls-files)
    if ($LASTEXITCODE -ne 0) { throw "无法读取项目依赖描述文件。" }
    $untrackedFiles = @(& git -C $RepositoryPath ls-files --others --exclude-standard)
    if ($LASTEXITCODE -ne 0) { throw "无法读取项目未跟踪依赖描述文件。" }
    $candidateFiles = @($trackedFiles + $untrackedFiles) |
        ForEach-Object { $_ -replace '\\', '/' } |
        Where-Object {
            $_ -match '(^|/)(settings|build)\.gradle(\.kts)?$' -or
            $_ -match '(^|/)gradle\.properties$' -or
            $_ -match '(^|/)gradle/wrapper/gradle-wrapper\.(properties|jar)$' -or
            $_ -match '(^|/)gradle/.*\.(toml|xml|lock|lockfile)$' -or
            $_ -match '(^|/)gradle\.lockfile$' -or
            $_ -match '(^|/)(buildSrc|build-logic)/.*\.(kt|kts|java|properties|toml)$'
        } |
        Sort-Object -Unique

    $digestLines = foreach ($relativePath in $candidateFiles) {
        $fullPath = Join-Path $RepositoryPath $relativePath
        if (Test-Path -LiteralPath $fullPath -PathType Leaf) {
            $fileHash = (Get-FileHash -LiteralPath $fullPath -Algorithm SHA256).Hash.ToLowerInvariant()
            "$relativePath=$fileHash"
        }
    }
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $digestBytes = [System.Text.Encoding]::UTF8.GetBytes(($digestLines -join "`n"))
        $digest = ($sha256.ComputeHash($digestBytes) | ForEach-Object { $_.ToString('x2') }) -join ''
    }
    finally {
        $sha256.Dispose()
    }
    return [ordered]@{
        sha256 = $digest
        file_count = @($digestLines).Count
    }
}

function Assert-SeparatePaths([string]$First, [string]$Second) {
    $comparison = [System.StringComparison]::OrdinalIgnoreCase
    if ($First.Equals($Second, $comparison) -or
        $First.StartsWith($Second + [System.IO.Path]::DirectorySeparatorChar, $comparison) -or
        $Second.StartsWith($First + [System.IO.Path]::DirectorySeparatorChar, $comparison)) {
        throw "项目、运行时和快照目录必须相互独立：$First / $Second"
    }
}

$project = Normalize-Path $ProjectPath
$snapshot = Normalize-Path $SnapshotPath
$wrapper = Join-Path $project "gradlew.bat"
$wrapperProperties = Join-Path $project "gradle\wrapper\gradle-wrapper.properties"
if (-not (Test-Path -LiteralPath $wrapper -PathType Leaf) -or
    -not (Test-Path -LiteralPath $wrapperProperties -PathType Leaf)) {
    throw "项目缺少 gradlew.bat 或 gradle-wrapper.properties：$project"
}
Assert-SeparatePaths $project $snapshot
if (Test-Path -LiteralPath $snapshot) {
    throw "快照目录已存在，请换用新的版本目录：$snapshot"
}
$javaHome = $env:JAVA_HOME
if (-not $javaHome -or -not (Test-Path -LiteralPath (Join-Path $javaHome 'bin\java.exe') -PathType Leaf)) {
    throw "预热需要有效的 JAVA_HOME，且本项目要求 JDK 17。"
}
$javaVersion = & (Join-Path $javaHome 'bin\java.exe') -version 2>&1 | Select-Object -First 1
if ($javaVersion -notmatch 'version "17\.0\.20\.1') {
    throw "可复现快照要求 JDK 17.0.20.1，当前版本：$javaVersion"
}
$sdkHome = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { $env:ANDROID_SDK_ROOT }
if (-not $sdkHome) {
    throw "预热需要 ANDROID_HOME 或 ANDROID_SDK_ROOT。"
}
$requiredSdkPaths = @(
    'platforms\android-34',
    'platforms\android-36',
    'build-tools\34.0.0',
    'build-tools\36.0.0',
    'ndk\28.2.13676358',
    'cmake\3.31.6',
    'cmake\3.31.6\bin\ninja.exe',
    'platform-tools\adb.exe',
    'cmdline-tools\latest\bin\sdkmanager.bat'
)
foreach ($relativePath in $requiredSdkPaths) {
    if (-not (Test-Path -LiteralPath (Join-Path $sdkHome $relativePath))) {
        throw "缺少已登记的 Android 工具链组件：$relativePath"
    }
}
$sourceCommit = (& git -C $project rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $sourceCommit -notmatch '^[0-9a-f]{40}$') {
    throw "无法读取项目 Git 提交。"
}
$sourceDirty = [bool](& git -C $project status --porcelain --untracked-files=no)
if ($LASTEXITCODE -ne 0) { throw "无法读取项目 Git 状态。" }
if ($sourceDirty) { throw "生成依赖快照前必须保持已跟踪源码干净。" }
$dependencyDescriptor = Get-DependencyDescriptorDigest $project

$tasks = @(
    ":app:assembleDebug",
    ":app:assembleRelease",
    ":app:bundleDebug",
    ":app:bundleRelease",
    ":app:assembleDebugAndroidTest",
    ":benchmark:assemble",
    ":adbcore:assembleDebug",
    ":adbcore:assembleRelease",
    ":univerge-heavy-drag-core:testClasses",
    ":univerge-heavy-drag-android:assembleDebug",
    ":univerge-heavy-drag-android:assembleRelease",
    ":univerge-heavy-drag-compose:assembleDebug",
    ":univerge-heavy-drag-compose:assembleRelease",
    ":univerge-overlay:assembleDebug",
    ":univerge-overlay:assembleRelease",
    ":univerge-accessibility:assembleDebug",
    ":univerge-accessibility:assembleRelease",
    ":univerge-heavy-drag-android:compileDebugUnitTestKotlin",
    ":univerge-heavy-drag-android:compileReleaseUnitTestKotlin",
    ":univerge-heavy-drag-compose:compileDebugUnitTestKotlin",
    ":univerge-heavy-drag-compose:compileReleaseUnitTestKotlin",
    ":univerge-overlay:compileDebugUnitTestKotlin",
    ":univerge-overlay:compileReleaseUnitTestKotlin",
    ":univerge-accessibility:compileDebugUnitTestKotlin",
    ":univerge-accessibility:compileReleaseUnitTestKotlin",
    ":univerge-core:generatePomFileForReleasePublication",
    ":univerge-core:generateMetadataFileForReleasePublication",
    ":univerge-heavy-drag-core:generatePomFileForReleasePublication",
    ":univerge-heavy-drag-core:generateMetadataFileForReleasePublication",
    ":univerge-overlay:generatePomFileForReleasePublication",
    ":univerge-overlay:generateMetadataFileForReleasePublication",
    ":univerge-accessibility:generatePomFileForReleasePublication",
    ":univerge-accessibility:generateMetadataFileForReleasePublication"
)
$testIsolation = Join-Path $PSScriptRoot "runtime-test-isolation.gradle"
$mirrorNormalization = Join-Path $PSScriptRoot "warm-mirrors.gradle"
$configurationWarmup = Join-Path $PSScriptRoot "warm-all-configurations.gradle"
if (-not (Test-Path -LiteralPath $testIsolation -PathType Leaf)) {
    throw "缺少测试 JVM 隔离脚本：$testIsolation"
}
if (-not (Test-Path -LiteralPath $mirrorNormalization -PathType Leaf)) {
    throw "缺少仓库镜像归一化脚本：$mirrorNormalization"
}
if (-not (Test-Path -LiteralPath $configurationWarmup -PathType Leaf)) {
    throw "缺少完整配置预热脚本：$configurationWarmup"
}
$blinkvoiceProject = Join-Path $project "blinkvoice-visual-sdk"
$touchProject = Join-Path $project "univerge-touch"
foreach ($nestedProject in @($blinkvoiceProject, $touchProject)) {
    if (-not (Test-Path -LiteralPath $nestedProject -PathType Container)) {
        throw "缺少独立构建根：$nestedProject"
    }
}

Push-Location $project
try {
    & $AgentCtlPath prepare --agent-id $AgentId --allow-primary | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "无法准备种子运行时。" }
    $agentEnv = & $AgentCtlPath env --agent-id $AgentId --format json | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw "无法读取种子运行时路径。" }
    $seedHome = Normalize-Path $agentEnv.GRADLE_USER_HOME
    $mavenLocal = Normalize-Path $agentEnv.MAVEN_REPO_LOCAL
    Assert-SeparatePaths $seedHome $snapshot
    Assert-SeparatePaths $mavenLocal $snapshot

    $seedPayloadPaths = @(
        (Join-Path $seedHome 'caches\modules-2'),
        (Join-Path $seedHome 'wrapper\dists'),
        (Join-Path $seedHome 'reports'),
        (Join-Path $mavenLocal 'org\robolectric')
    )
    foreach ($seedPayloadPath in $seedPayloadPaths) {
        if (Test-Path -LiteralPath $seedPayloadPath -PathType Container) {
            $existingEntry = Get-ChildItem -LiteralPath $seedPayloadPath -Force | Select-Object -First 1
            if ($existingEntry) {
                throw "种子缓存不是空目录，请使用新的 AgentId 或先执行受控清理：$seedPayloadPath"
            }
        }
    }

    $rootWarmReport = Join-Path $seedHome "reports\root-configurations.json"
    $env:AGENT_WARM_REPORT = $rootWarmReport
    try {
        & $AgentCtlPath run-gradle --agent-id $AgentId --allow-primary -- --no-daemon -I $configurationWarmup warmAllConfigurations
        if ($LASTEXITCODE -ne 0) { throw "主工程完整配置预热失败；报告：$rootWarmReport" }
    }
    finally {
        Remove-Item Env:AGENT_WARM_REPORT -ErrorAction SilentlyContinue
    }

    & $AgentCtlPath run-gradle --agent-id $AgentId --allow-primary -- --no-daemon @tasks
    if ($LASTEXITCODE -ne 0) { throw "预热构建失败；已下载内容保留在种子运行时，未发布快照。" }
    & $AgentCtlPath run-gradle --agent-id $AgentId --allow-primary -- --no-daemon '-Pkotlin.compiler.execution.strategy=in-process' -I $testIsolation ":univerge-heavy-drag-compose:testDebugUnitTest"
    if ($LASTEXITCODE -ne 0) { throw "Robolectric 预热测试失败；已下载内容保留在种子运行时，未发布快照。" }
    & $AgentCtlPath run-gradle --agent-id $AgentId --allow-primary -- --no-daemon '-Pkotlin.compiler.execution.strategy=in-process' -I $testIsolation ":gesture-server:testDebugUnitTest"
    if ($LASTEXITCODE -ne 0) { throw "默认 SDK Robolectric 预热测试失败；已下载内容保留在种子运行时，未发布快照。" }
    & $AgentCtlPath run-gradle --agent-id $AgentId --allow-primary -- --no-daemon '-Pkotlin.compiler.execution.strategy=in-process' -I $testIsolation ":univerge-overlay:testDebugUnitTest"
    if ($LASTEXITCODE -ne 0) { throw "Overlay Robolectric 预热测试失败；已下载内容保留在种子运行时，未发布快照。" }

    $pinnedGradle = Get-ChildItem -LiteralPath (Join-Path $seedHome "wrapper\dists\gradle-8.10.2-all") -Recurse -Filter "gradle.bat" |
        Select-Object -First 1 -ExpandProperty FullName
    if (-not $pinnedGradle) {
        throw "主工程构建后仍未找到固定 Gradle 8.10.2 启动器。"
    }
    $previousGradleHome = $env:GRADLE_USER_HOME
    $previousMavenLocal = $env:MAVEN_REPO_LOCAL
    $env:GRADLE_USER_HOME = $seedHome
    $env:MAVEN_REPO_LOCAL = $mavenLocal
    try {
        & $AgentCtlPath lock-build --agent-id $AgentId --lease 3600 | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "无法获取独立构建根的预热锁。" }
        try {
            Push-Location $blinkvoiceProject
            try {
                $env:AGENT_WARM_REPORT = Join-Path $seedHome "reports\blinkvoice-configurations.json"
                & $pinnedGradle --no-daemon "-Dmaven.repo.local=$mavenLocal" -I $mirrorNormalization -I $configurationWarmup warmAllConfigurations
                if ($LASTEXITCODE -ne 0) { throw "Blinkvoice 完整配置预热失败。" }
                Remove-Item Env:AGENT_WARM_REPORT -ErrorAction SilentlyContinue
                & $pinnedGradle --no-daemon '-Pkotlin.compiler.execution.strategy=in-process' "-Dmaven.repo.local=$mavenLocal" -I $mirrorNormalization -I $testIsolation assembleDebug assembleRelease testDebugUnitTest testReleaseUnitTest
                if ($LASTEXITCODE -ne 0) { throw "Blinkvoice 预热失败。" }
            }
            finally {
                Remove-Item Env:AGENT_WARM_REPORT -ErrorAction SilentlyContinue
                Pop-Location
            }

            Push-Location $touchProject
            try {
                $env:AGENT_WARM_REPORT = Join-Path $seedHome "reports\touch-configurations.json"
                & $pinnedGradle --no-daemon "-Dmaven.repo.local=$mavenLocal" -I $mirrorNormalization -I $configurationWarmup warmAllConfigurations
                if ($LASTEXITCODE -ne 0) { throw "Touch 完整配置预热失败。" }
                Remove-Item Env:AGENT_WARM_REPORT -ErrorAction SilentlyContinue
                & $pinnedGradle --no-daemon '-Pkotlin.compiler.execution.strategy=in-process' "-Dmaven.repo.local=$mavenLocal" -I $mirrorNormalization assembleDebug assembleRelease generatePomFileForReleasePublication generateMetadataFileForReleasePublication
                if ($LASTEXITCODE -ne 0) { throw "Touch Debug/Release 预热失败。" }
            }
            finally {
                Remove-Item Env:AGENT_WARM_REPORT -ErrorAction SilentlyContinue
                Pop-Location
            }
        }
        finally {
            Push-Location $project
            try {
                & $AgentCtlPath unlock-build --agent-id $AgentId | Out-Null
            }
            finally {
                Pop-Location
            }
        }
    }
    finally {
        $env:GRADLE_USER_HOME = $previousGradleHome
        $env:MAVEN_REPO_LOCAL = $previousMavenLocal
    }

    $modulesCache = Join-Path $seedHome "caches\modules-2"
    $wrapperDists = Join-Path $seedHome "wrapper\dists"
    $robolectricCache = Join-Path $mavenLocal "org\robolectric"
    if (-not (Test-Path -LiteralPath $modulesCache -PathType Container) -or
        -not (Test-Path -LiteralPath $wrapperDists -PathType Container) -or
        -not (Test-Path -LiteralPath $robolectricCache -PathType Container)) {
        throw "种子构建缺少 Gradle 缓存、Wrapper 发行包或 Robolectric 测试运行时。"
    }

    $staging = $snapshot + ".staging"
    if (Test-Path -LiteralPath $staging) { throw "暂存目录已存在：$staging" }
    New-Item -ItemType Directory -Path (Split-Path -Parent $staging) -Force | Out-Null
    New-Item -ItemType Directory -Path $staging | Out-Null
    try {
        $dependencyTarget = Join-Path $staging "gradle-dependencies\modules-2"
        New-Item -ItemType Directory -Path $dependencyTarget -Force | Out-Null
        & robocopy $modulesCache $dependencyTarget /E /XF *.lock gc.properties /NFL /NDL /NJH /NJS /NP | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "复制 Gradle 依赖失败，Robocopy 退出码：$LASTEXITCODE" }

        $distributionTarget = Join-Path $staging "gradle-distribution"
        New-Item -ItemType Directory -Path $distributionTarget -Force | Out-Null
        & robocopy $wrapperDists $distributionTarget /E /XF *.lck /NFL /NDL /NJH /NJS /NP | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "复制 Gradle 发行包失败，Robocopy 退出码：$LASTEXITCODE" }

        $mavenTarget = Join-Path $staging "maven-test-runtime\org\robolectric"
        New-Item -ItemType Directory -Path $mavenTarget -Force | Out-Null
        & robocopy $robolectricCache $mavenTarget /E /XF *.lock *.lck /NFL /NDL /NJH /NJS /NP | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "复制 Robolectric 测试运行时失败，Robocopy 退出码：$LASTEXITCODE" }

        $reportTarget = Join-Path $staging "configuration-reports"
        New-Item -ItemType Directory -Path $reportTarget -Force | Out-Null
        $configurationReports = @(
            $rootWarmReport,
            (Join-Path $seedHome 'reports\blinkvoice-configurations.json'),
            (Join-Path $seedHome 'reports\touch-configurations.json')
        )
        foreach ($configurationReport in $configurationReports) {
            if (-not (Test-Path -LiteralPath $configurationReport -PathType Leaf)) {
                throw "种子构建缺少配置解析报告：$configurationReport"
            }
            Copy-Item -LiteralPath $configurationReport -Destination $reportTarget
        }

        $configurationSummary = @()
        foreach ($configurationReport in $configurationReports) {
            $reportData = Get-Content -LiteralPath $configurationReport -Raw | ConvertFrom-Json
            $configurationSummary += [ordered]@{
                report = Split-Path -Leaf $configurationReport
                resolved = @($reportData.resolved).Count
                skipped = @($reportData.skipped).Count
                failed = @($reportData.failed).Count
            }
        }

        $payloadFiles = Get-ChildItem -LiteralPath $staging -File -Recurse
        $payloadBytes = ($payloadFiles | Measure-Object -Property Length -Sum).Sum
        $manifest = [ordered]@{
            schema_version = 1
            profile_id = Split-Path -Leaf $snapshot
            coverage = "main-blinkvoice-touch-configuration-scan-debug-release-bundle-android-test-publication-and-robolectric-runtime"
            created_at = [DateTime]::UtcNow.ToString("o")
            project = $project
            source_commit = $sourceCommit
            source_dirty = $sourceDirty
            dependency_descriptor_sha256 = $dependencyDescriptor.sha256
            dependency_descriptor_file_count = $dependencyDescriptor.file_count
            wrapper_properties_sha256 = (Get-FileHash -LiteralPath $wrapperProperties -Algorithm SHA256).Hash.ToLowerInvariant()
            java_version = $javaVersion.ToString()
            android_sdk_root = (Normalize-Path $sdkHome)
            toolchain_inventory = "../toolchain-inventory.json"
            toolchain_bundled = $false
            gradle_user_home = $seedHome
            maven_local = $mavenLocal
            tasks = $tasks
            categories = @("gradle-dependencies", "gradle-distribution", "maven-test-runtime", "configuration-reports")
            payload_file_count = $payloadFiles.Count
            payload_content_bytes = $payloadBytes
            test_runtime_scope = "org/robolectric only; excludes locally published candidate artifacts"
            configuration_summary = $configurationSummary
            profiles = @(
                [ordered]@{
                    build_root = "."
                    gradle = "8.10.2"
                    verified_tasks = $tasks + @(
                        ":univerge-heavy-drag-compose:testDebugUnitTest",
                        ":gesture-server:testDebugUnitTest",
                        ":univerge-overlay:testDebugUnitTest",
                        "warmAllConfigurations"
                    )
                },
                [ordered]@{
                    build_root = "blinkvoice-visual-sdk"
                    gradle = "8.10.2"
                    requires_init_script = "scripts/warm-mirrors.gradle"
                    verified_tasks = @("warmAllConfigurations", "assembleDebug", "assembleRelease", "testDebugUnitTest", "testReleaseUnitTest")
                },
                [ordered]@{
                    build_root = "univerge-touch"
                    gradle = "8.10.2"
                    repository_wrapper = "8.7"
                    repository_wrapper_status = "incompatible-with-agp-8.7.3"
                    requires_init_script = "scripts/warm-mirrors.gradle"
                    verified_tasks = @("warmAllConfigurations", "assembleDebug", "assembleRelease", "generatePomFileForReleasePublication", "generateMetadataFileForReleasePublication")
                    blocked_tasks = @("testDebugUnitTest", "testReleaseUnitTest")
                    blocked_reason = "Existing Java test source and namespace migration errors; dependency download is complete"
                }
            )
            unverified = @("connected Android device tests", "benchmark device execution", "Touch unit tests because the repository currently has source errors")
        }
        $manifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $staging "manifest.json") -Encoding utf8
        Move-Item -LiteralPath $staging -Destination $snapshot
    }
    catch {
        if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
        throw
    }
    Write-Output "依赖快照已生成：$snapshot"
    Write-Output "每个 Worktree 仍需独立的 Gradle 用户目录、Maven 仓库和构建输出。"
}
finally {
    Pop-Location
}
