param(
    [Parameter(Mandatory = $true)] [string]$ProjectPath,
    [Parameter(Mandatory = $true)] [string]$SeedPath,
    [Parameter(Mandatory = $true)] [string]$SnapshotPath,
    [Parameter(Mandatory = $true)] [string[]]$Tasks,
    [Parameter(Mandatory = $true)] [string]$WrapperJarPath
)

$ErrorActionPreference = 'Stop'
$project = [IO.Path]::GetFullPath($ProjectPath).TrimEnd('\')
$seed = [IO.Path]::GetFullPath($SeedPath).TrimEnd('\')
$snapshot = [IO.Path]::GetFullPath($SnapshotPath).TrimEnd('\')
$wrapper = [IO.Path]::GetFullPath($WrapperJarPath)
foreach ($path in @($seed, $snapshot)) {
    if ($path.Equals($project, [StringComparison]::OrdinalIgnoreCase) -or
        $path.StartsWith($project + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw "缓存目录不能位于项目源码内：$path"
    }
    if (Test-Path -LiteralPath $path) { throw "目录已存在，拒绝覆盖：$path" }
}
if (-not (Test-Path -LiteralPath $wrapper -PathType Leaf)) { throw "缺少 Gradle Wrapper JAR：$wrapper" }
$java = Join-Path $env:JAVA_HOME 'bin\java.exe'
if (-not (Test-Path -LiteralPath $java -PathType Leaf)) { throw '需要有效的 JAVA_HOME' }
$version = & $java -version 2>&1 | Select-Object -First 1
if ($version -notmatch 'version "17\.') { throw "本项目要求 JDK 17：$version" }
$properties = Join-Path $project 'gradle\wrapper\gradle-wrapper.properties'
if (-not (Test-Path -LiteralPath $properties -PathType Leaf)) { throw "缺少 Wrapper 配置：$properties" }

New-Item -ItemType Directory -Path $seed -Force | Out-Null
$priorGradle = $env:GRADLE_USER_HOME
$priorMaven = $env:MAVEN_REPO_LOCAL
$env:GRADLE_USER_HOME = Join-Path $seed 'gradle-user-home'
$env:MAVEN_REPO_LOCAL = Join-Path $seed 'maven-local'
$mavenArgument = '-Dmaven.repo.local=' + $env:MAVEN_REPO_LOCAL
try {
    Push-Location $project
    try {
        & $java $mavenArgument -classpath $wrapper 'org.gradle.wrapper.GradleWrapperMain' '--no-daemon' @Tasks
        if ($LASTEXITCODE -ne 0) { throw "独立 Gradle 构建失败，退出码：$LASTEXITCODE" }
    }
    finally { Pop-Location }

    $sourceModules = Join-Path $env:GRADLE_USER_HOME 'caches\modules-2'
    $sourceDists = Join-Path $env:GRADLE_USER_HOME 'wrapper\dists'
    if (-not (Test-Path $sourceModules) -or -not (Test-Path $sourceDists)) { throw 'Gradle 缓存尚未形成' }
    $stage = $snapshot + '.staging'
    New-Item -ItemType Directory -Path (Join-Path $stage 'gradle-dependencies\modules-2') -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $stage 'gradle-distribution') -Force | Out-Null
    & robocopy $sourceModules (Join-Path $stage 'gradle-dependencies\modules-2') /E /XF *.lock gc.properties /NFL /NDL /NJH /NJS /NP | Out-Null
    if ($LASTEXITCODE -ge 8) { throw '复制依赖缓存失败' }
    & robocopy $sourceDists (Join-Path $stage 'gradle-distribution') /E /XF *.lck /NFL /NDL /NJH /NJS /NP | Out-Null
    if ($LASTEXITCODE -ge 8) { throw '复制 Gradle 发行版失败' }
    $manifest = [ordered]@{
        schema_version = 1
        coverage = 'standalone-build-verified'
        project = $project
        wrapper_properties_sha256 = (Get-FileHash $properties -Algorithm SHA256).Hash.ToLowerInvariant()
        tasks = $Tasks
        created_at = [DateTime]::UtcNow.ToString('o')
    }
    $manifest | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $stage 'manifest.json') -Encoding utf8
    Move-Item -LiteralPath $stage -Destination $snapshot
    Write-Output "独立构建缓存快照：$snapshot"
}
finally {
    $env:GRADLE_USER_HOME = $priorGradle
    $env:MAVEN_REPO_LOCAL = $priorMaven
}
