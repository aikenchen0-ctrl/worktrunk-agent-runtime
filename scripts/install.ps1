#Requires -Version 5.1
param(
    [string]$PackageRoot = (Split-Path -Parent $PSScriptRoot),
    [string]$InstallRoot = (Join-Path $env:LOCALAPPDATA 'agent-runtime')
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$lock = $null
$stage = $null
$promoted = @()
$success = $false

function Assert-NoReparsePoint([string]$Path) {
    $cursor = [IO.Path]::GetFullPath($Path)
    while ($cursor) {
        if (Test-Path -LiteralPath $cursor) {
            if ((Get-Item -LiteralPath $cursor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw '安装路径或软件包中存在符号链接或目录联接，已拒绝。'
            }
        }
        $cursor = [IO.Path]::GetDirectoryName($cursor)
    }
}

try {
    # 安装只允许新增公开程序材料，不覆盖运行状态、旧程序或用户自定义 Skill。
    foreach ($path in @($InstallRoot, $PackageRoot)) {
        if ($path -notmatch '^[A-Za-z]:[\\/]' -or $path -match '(^|[\\/])\.\.([\\/]|$)') {
            throw '安装目录和软件包目录必须是本机磁盘绝对路径，不能含上级跳转。'
        }
        Assert-NoReparsePoint $path
    }
    $InstallRoot = [IO.Path]::GetFullPath($InstallRoot).TrimEnd('\', '/')
    $PackageRoot = [IO.Path]::GetFullPath($PackageRoot).TrimEnd('\', '/')
    if ($InstallRoot.Length -le 3 -or $InstallRoot -eq $PackageRoot -or
        $InstallRoot.StartsWith($PackageRoot + '\', [StringComparison]::OrdinalIgnoreCase) -or
        $PackageRoot.StartsWith($InstallRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw '安装目录不能是磁盘根目录，也不能与软件包目录互相包含。'
    }
    $manifestPath = Join-Path $PackageRoot 'package-manifest.json'
    Assert-NoReparsePoint $manifestPath
    $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($manifest.schema_version -ne 1 -or $manifest.platform -ne 'windows-x86_64' -or
        $manifest.version -notmatch '^\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?$') {
        throw '软件包清单版本或平台不受支持。'
    }
    $allowed = @('bin', 'scripts', 'skills', 'examples', 'docs', 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'CHANGELOG.md', 'SECURITY.md')
    $seen = @{}
    $entries = @($manifest.files)
    if ($entries.Count -eq 0) { throw '软件包清单不能为空。' }
    foreach ($entry in $entries) {
        $relative = $entry.path
        if ($relative -isnot [string] -or $relative -notmatch '^[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*$' -or
            $relative -match '(^|/)\.{1,2}(/|$)' -or $allowed -notcontains $relative.Split('/')[0] -or
            $seen.ContainsKey($relative) -or $entry.sha256 -notmatch '^[a-fA-F0-9]{64}$') {
            throw '软件包存在非法、重复或越界的文件条目。'
        }
        $seen[$relative] = $true
        $source = Join-Path $PackageRoot $relative
        Assert-NoReparsePoint $source
        if (-not (Test-Path -LiteralPath $source -PathType Leaf) -or
            (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -ne $entry.sha256) {
            throw "软件包校验失败：$relative"
        }
    }
    foreach ($required in @('bin/agentctl.exe', 'scripts/worktrunk-hook.ps1', 'scripts/install.ps1', 'LICENSE-MIT', 'LICENSE-APACHE')) {
        if (-not $seen.ContainsKey($required)) { throw "软件包缺少必要文件：$required" }
    }
    $tops = @($entries | ForEach-Object { $_.path.Split('/')[0] } | Sort-Object -Unique)
    New-Item -ItemType Directory -Path $InstallRoot -Force | Out-Null
    $lockPath = Join-Path $InstallRoot '.install.lock'
    $lock = [IO.File]::Open($lockPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    foreach ($top in ($tops + 'package-manifest.json')) {
        if (Test-Path -LiteralPath (Join-Path $InstallRoot $top)) {
            throw '安装目标已有程序材料；禁止静默覆盖。请选择新的安装目录并按发布指南迁移。'
        }
    }
    $stage = Join-Path $InstallRoot ('.install-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stage | Out-Null
    foreach ($entry in $entries) {
        $destination = Join-Path $stage $entry.path
        New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $PackageRoot $entry.path) -Destination $destination
        if ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -ne $entry.sha256) {
            throw '复制后校验失败，未安装软件包。'
        }
    }
    Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $stage 'package-manifest.json')
    foreach ($top in ($tops + 'package-manifest.json')) {
        $destination = Join-Path $InstallRoot $top
        # top 来自固定白名单；这里只移动本次创建的暂存子项。
        Move-Item -LiteralPath (Join-Path $stage $top) -Destination $destination
        $promoted += $destination
    }
    $success = $true
    Write-Output "已安装 agentctl $($manifest.version)：$(Join-Path $InstallRoot 'bin\agentctl.exe')"
    Write-Output '未修改 PATH、运行状态、业务仓库或用户 Skill 目录。'
} catch {
    [Console]::Error.WriteLine("安装失败：$($_.Exception.Message)")
} finally {
    # 失败时仅回收本次新增的目标；不触碰原有缓存与租约。
    if (-not $success) {
        foreach ($path in $promoted) {
            if ([IO.Path]::GetDirectoryName($path) -eq $InstallRoot) {
                Remove-Item -LiteralPath $path -Recurse -Force -ErrorAction SilentlyContinue
            }
        }
    }
    if ($stage -and [IO.Path]::GetDirectoryName($stage) -eq $InstallRoot -and (Test-Path -LiteralPath $stage)) {
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
    if ($lock) { $lock.Dispose(); Remove-Item -LiteralPath $lockPath -Force }
}
if (-not $success) { exit 1 }
