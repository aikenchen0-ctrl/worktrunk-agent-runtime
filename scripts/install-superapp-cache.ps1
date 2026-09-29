param(
    [Parameter(Mandatory = $true)]
    [string]$SnapshotPath,

    [string]$AgentCtlPath = "agentctl",

    [string]$WorktreePath = (Get-Location).Path,

    [string]$AgentId = $env:AGENT_ID,

    [string]$RuntimeRoot = $env:AGENT_RUNTIME_ROOT,

    [string]$JavaHome = $env:JAVA_HOME,

    [string]$AndroidSdkRoot = $(if ($env:ANDROID_SDK_ROOT) { $env:ANDROID_SDK_ROOT } else { $env:ANDROID_HOME }),

    [string[]]$VerifyTasks = @(),

    [switch]$AllowSourceDrift
)

$ErrorActionPreference = "Stop"

function Get-DependencyDescriptorDigest([string]$RepositoryPath) {
    $trackedFiles = @(& git -C $RepositoryPath ls-files)
    if ($LASTEXITCODE -ne 0) { throw "无法读取目标 Worktree 的依赖描述文件。" }
    $untrackedFiles = @(& git -C $RepositoryPath ls-files --others --exclude-standard)
    if ($LASTEXITCODE -ne 0) { throw "无法读取目标 Worktree 的未跟踪依赖描述文件。" }
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

foreach ($requiredValue in @{
    AgentId = $AgentId
    RuntimeRoot = $RuntimeRoot
    JavaHome = $JavaHome
    AndroidSdkRoot = $AndroidSdkRoot
}.GetEnumerator()) {
    if ([string]::IsNullOrWhiteSpace($requiredValue.Value)) {
        throw "缺少参数或环境变量：$($requiredValue.Key)"
    }
}

$snapshot = [System.IO.Path]::GetFullPath($SnapshotPath)
$worktree = [System.IO.Path]::GetFullPath($WorktreePath)
$manifestPath = Join-Path $snapshot "manifest.json"
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    throw "缓存快照缺少 manifest.json：$snapshot"
}
if (-not (Test-Path -LiteralPath (Join-Path $worktree '.git'))) {
    throw "目标目录不是 Git Worktree：$worktree"
}
if (-not (Test-Path -LiteralPath (Join-Path $JavaHome 'bin\java.exe') -PathType Leaf)) {
    throw "JAVA_HOME 缺少 java.exe：$JavaHome"
}
if (-not (Test-Path -LiteralPath (Join-Path $AndroidSdkRoot 'platform-tools\adb.exe') -PathType Leaf)) {
    throw "Android SDK 缺少 platform-tools：$AndroidSdkRoot"
}

$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if ($manifest.schema_version -ne 1) {
    throw "当前 agentctl 不支持该快照格式：$($manifest.schema_version)"
}
$dependencyDescriptor = Get-DependencyDescriptorDigest $worktree
if (-not $AllowSourceDrift -and $manifest.dependency_descriptor_sha256 -and
    $dependencyDescriptor.sha256 -ne $manifest.dependency_descriptor_sha256) {
    throw "目标 Worktree 的依赖描述文件与快照不一致，请生成新快照。"
}
$payloadFiles = Get-ChildItem -LiteralPath $snapshot -File -Recurse |
    Where-Object FullName -ne $manifestPath
$payloadBytes = ($payloadFiles | Measure-Object -Property Length -Sum).Sum
if ($payloadFiles.Count -ne [int64]$manifest.payload_file_count -or
    [int64]$payloadBytes -ne [int64]$manifest.payload_content_bytes) {
    throw "缓存快照载荷数量或容量与 manifest.json 不一致。"
}

$env:AGENT_ID = $AgentId
$env:AGENT_RUNTIME_ROOT = [System.IO.Path]::GetFullPath($RuntimeRoot)
$env:JAVA_HOME = [System.IO.Path]::GetFullPath($JavaHome)
$env:ANDROID_SDK_ROOT = [System.IO.Path]::GetFullPath($AndroidSdkRoot)
$env:ANDROID_HOME = $env:ANDROID_SDK_ROOT
$env:PATH = "$env:JAVA_HOME\bin;$env:ANDROID_SDK_ROOT\platform-tools;$env:PATH"

Push-Location $worktree
try {
    & $AgentCtlPath prepare --agent-id $AgentId
    if ($LASTEXITCODE -ne 0) { throw "Agent 运行时准备失败，退出码：$LASTEXITCODE" }

    & $AgentCtlPath install-cache --agent-id $AgentId --snapshot $snapshot
    if ($LASTEXITCODE -ne 0) { throw "缓存快照导入失败，退出码：$LASTEXITCODE" }

    & $AgentCtlPath doctor --agent-id $AgentId
    if ($LASTEXITCODE -ne 0) { throw "运行环境诊断失败，退出码：$LASTEXITCODE" }

    if ($VerifyTasks.Count -gt 0) {
        & $AgentCtlPath run-gradle --agent-id $AgentId -- --offline --no-daemon '-Pkotlin.compiler.execution.strategy=in-process' @VerifyTasks
        if ($LASTEXITCODE -ne 0) { throw "缓存离线验证失败，退出码：$LASTEXITCODE" }
    }

    & $AgentCtlPath env --agent-id $AgentId --format json
}
finally {
    Pop-Location
}
