#Requires -Version 5.1
param(
    [string]$TargetDir = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target'),
    [string]$OutputRoot = (Join-Path (Split-Path -Parent $PSScriptRoot) 'dist'),
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path -Parent $PSScriptRoot
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or -not [Environment]::Is64BitProcess) { throw '此发布包仅支持 Windows x64。' }
$versionMatch = [regex]::Match((Get-Content -LiteralPath (Join-Path $root 'Cargo.toml') -Raw -Encoding UTF8), '(?m)^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success -or $versionMatch.Groups[1].Value -notmatch '^\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?$') { throw 'Cargo 版本无效。' }
$version = $versionMatch.Groups[1].Value
Push-Location $root
try {
    if (-not $SkipBuild) {
        & cargo build --release --locked --target-dir $TargetDir
        if ($LASTEXITCODE -ne 0) { throw '发布构建失败，已停止打包。' }
    }
    $binary = Join-Path $TargetDir 'release\agentctl.exe'
    $actualVersion = & $binary --version
    if ($LASTEXITCODE -ne 0 -or $actualVersion -ne "agentctl $version") { throw '二进制版本与 Cargo 不一致。' }
    $OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
    New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
    $name = "agent-runtime-$version-windows-x86_64"
    $package = Join-Path $OutputRoot $name
    $zip = Join-Path $OutputRoot "$name.zip"
    if ((Test-Path -LiteralPath $package) -or (Test-Path -LiteralPath $zip)) { throw '发布输出已存在；请选择新的 OutputRoot，不覆盖已有制品。' }
    New-Item -ItemType Directory -Path (Join-Path $package 'bin') -Force | Out-Null
    Copy-Item -LiteralPath $binary -Destination (Join-Path $package 'bin\agentctl.exe')
    # 不从仓库根递归复制，避免带入运行时、缓存、凭证和未审查的大文件。
    $paths = @('scripts', 'skills', 'examples', 'docs', 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'CHANGELOG.md', 'SECURITY.md')
    foreach ($relative in $paths) {
        $source = Join-Path $root $relative
        $items = @((Get-Item -LiteralPath $source -Force))
        if ($items[0].PSIsContainer) { $items += @(Get-ChildItem -LiteralPath $source -Recurse -Force) }
        foreach ($item in $items) {
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw '发布材料不能包含符号链接或目录联接。' }
            if (-not $item.PSIsContainer -and $item.Extension -notin @('.md', '.ps1', '.gradle', '.json', '.yml', '.yaml', '.toml', '')) { throw "发布材料含未授权文件类型：$($item.Name)" }
        }
        Copy-Item -LiteralPath $source -Destination (Join-Path $package $relative) -Recurse
    }
    $files = @(Get-ChildItem -LiteralPath $package -Recurse -File | Sort-Object FullName | ForEach-Object {
        [ordered]@{ path = $_.FullName.Substring($package.Length + 1).Replace('\', '/'); sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
    })
    $manifest = [ordered]@{ schema_version = 1; version = $version; platform = 'windows-x86_64'; files = $files }
    $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $package 'package-manifest.json') -Encoding UTF8
    Compress-Archive -LiteralPath $package -DestinationPath $zip -CompressionLevel Optimal
    $digest = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    "$digest  $name.zip" | Set-Content -LiteralPath "$zip.sha256" -Encoding ASCII
    Write-Output "已生成发布包：$zip"
    Write-Output "SHA-256：$digest"
    Write-Output '尚未签名、上传或创建远端 Release；失败输出须人工检查，禁止直接发布。'
} finally { Pop-Location }
