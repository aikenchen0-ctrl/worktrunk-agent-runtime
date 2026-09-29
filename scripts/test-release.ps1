#Requires -Version 5.1
param([Parameter(Mandatory = $true)][string]$PackageRoot)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$PackageRoot = (Resolve-Path -LiteralPath $PackageRoot).Path
$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\', '/')
$temp = Join-Path $tempBase ('agent-runtime-release-test-' + [guid]::NewGuid().ToString('N'))
$installer = Join-Path $PackageRoot 'scripts\install.ps1'
$hostExe = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$count = 0

function Assert-Test([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Invoke-Install([string]$Source, [string]$Destination, [bool]$ExpectedSuccess) {
    $result = Invoke-Child @('-File', $installer, '-PackageRoot', $Source, '-InstallRoot', $Destination)
    Assert-Test (($result.Code -eq 0) -eq $ExpectedSuccess) "安装返回码不符合预期：$($result.Code)；$($result.Output)"
}

function Invoke-Child([string[]]$Arguments, [string]$InputText = '') {
    # 直接收集子进程流，避免 PowerShell 5.1 将预期失败的标准错误变成终止异常。
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $hostExe
    $info.Arguments = '-NoProfile -NonInteractive ' + (($Arguments | ForEach-Object { '"' + $_.Replace('"', '\"') + '"' }) -join ' ')
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $info
    try {
        [void]$process.Start()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.StandardInput.Write($InputText)
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(30000)) { $process.Kill(); throw '测试子进程超时。' }
        return @{ Code = $process.ExitCode; Output = $stdout.Result + $stderr.Result }
    } finally { $process.Dispose() }
}

try {
    New-Item -ItemType Directory -Path $temp | Out-Null
    $install = Join-Path $temp 'install with spaces'
    $state = Join-Path $install 'worktrees\sentinel.txt'
    New-Item -ItemType Directory -Path (Split-Path -Parent $state) -Force | Out-Null
    Set-Content -LiteralPath $state -Value 'keep-runtime-state' -Encoding ASCII
    Invoke-Install $PackageRoot $install $true
    Assert-Test ((Get-Content -LiteralPath $state -Raw).Trim() -eq 'keep-runtime-state') '安装破坏了已有运行状态。'
    $exe = Join-Path $install 'bin\agentctl.exe'
    $version = & $exe --version
    Assert-Test ($LASTEXITCODE -eq 0 -and $version -match '^agentctl \d+\.\d+\.\d+') '安装后的版本命令失败。'
    $count++

    $before = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
    Invoke-Install $PackageRoot $install $false
    Assert-Test ((Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash -eq $before) '重复安装覆盖了旧程序。'
    Assert-Test ((Get-Content -LiteralPath $state -Raw).Trim() -eq 'keep-runtime-state') '拒绝覆盖时破坏了运行状态。'
    $count++

    $bad = Join-Path $temp 'tampered-package'
    Copy-Item -LiteralPath $PackageRoot -Destination $bad -Recurse
    Add-Content -LiteralPath (Join-Path $bad 'README.md') -Value 'tampered'
    $rejected = Join-Path $temp 'rejected-install'
    Invoke-Install $bad $rejected $false
    Assert-Test (-not (Test-Path -LiteralPath (Join-Path $rejected 'bin'))) '篡改包不应写入可执行文件。'
    $count++

    $manifestFile = Join-Path $bad 'package-manifest.json'
    $manifest = Get-Content -LiteralPath $manifestFile -Raw -Encoding UTF8 | ConvertFrom-Json
    $manifest.files[0].path = '../escape.txt'
    $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestFile -Encoding UTF8
    Invoke-Install $bad $rejected $false
    Assert-Test (-not (Test-Path -LiteralPath (Join-Path $temp 'escape.txt'))) '越界路径被安装器写入。'
    $count++

    $locked = Join-Path $temp 'locked-install'
    New-Item -ItemType Directory -Path $locked | Out-Null
    $held = [IO.File]::Open((Join-Path $locked '.install.lock'), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        Invoke-Install $PackageRoot $locked $false
        Assert-Test (-not (Test-Path -LiteralPath (Join-Path $locked 'bin'))) '并行安装锁未生效。'
    } finally { $held.Dispose() }
    $count++

    # 用模拟进程验证 Hook 返回码；不启动真实 Gradle 或连接手机。
    $fake = Join-Path $temp 'failed-command.cmd'
    Set-Content -LiteralPath $fake -Value '@exit /b 23' -Encoding ASCII
    $hook = Join-Path $install 'scripts\worktrunk-hook.ps1'
    foreach ($event in @('pre-start', 'pre-merge', 'pre-remove')) {
        $result = Invoke-Child @('-File', $hook, '-Event', $event, '-AgentCtl', $fake)
        Assert-Test ($result.Code -eq 23) "Hook 未保留退出码：$event"
        $count++
    }

    $capture = Join-Path $temp 'captured-arguments.json'
    $recording = Join-Path $temp 'recording-command.ps1'
    $recordingCode = 'ConvertTo-Json -InputObject @($args) | Set-Content -LiteralPath $env:HOOK_CAPTURE -Encoding UTF8; exit 17'
    Set-Content -LiteralPath $recording -Value $recordingCode -Encoding UTF8
    $oldCapture = $env:HOOK_CAPTURE
    $env:HOOK_CAPTURE = $capture
    try {
        $specialPath = "C:\worktree\feature'; throw 42;#"
        $json = @{ worktree_path = $specialPath; branch = 'ignored-branch' } | ConvertTo-Json -Compress
        $result = Invoke-Child @('-File', $hook, '-Event', 'post-remove', '-AgentCtl', $recording) $json
        Assert-Test ($result.Code -eq 17) '删除 Hook 未保留子命令状态。'
        $captured = Get-Content -LiteralPath $capture -Raw -Encoding UTF8 | ConvertFrom-Json
        Assert-Test ($captured.Count -eq 4 -and $captured[0] -eq 'cleanup' -and $captured[3] -eq $specialPath) "删除路径未作为单个字面参数传递：$($captured | ConvertTo-Json -Compress)"
        $count++
        $result = Invoke-Child @('-File', $hook, '-Event', 'post-remove', '-AgentCtl', $fake) '{"worktree_path":"relative-path"}'
        Assert-Test ($result.Code -eq 1) '删除 Hook 未拒绝非法上下文。'
        $count++
    } finally { $env:HOOK_CAPTURE = $oldCapture }

    # 所有 PowerShell 公开脚本都必须能被 Windows PowerShell 5.1 解析。
    foreach ($script in Get-ChildItem -LiteralPath (Join-Path $install 'scripts') -Filter '*.ps1') {
        $tokens = $null
        $errors = $null
        [Management.Automation.Language.Parser]::ParseFile($script.FullName, [ref]$tokens, [ref]$errors) | Out-Null
        Assert-Test ($errors.Count -eq 0) "脚本语法错误：$($script.Name)"
    }
    $count++
    Write-Output "发布包回归通过：$count 项；模拟 Hook 不代表 Android 或远端端到端验收。"
} finally {
    # 仅删除本轮随机生成、位于系统临时目录下的测试目录。
    if ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($temp)) -eq $tempBase -and
        (Split-Path -Leaf $temp) -like 'agent-runtime-release-test-*' -and (Test-Path -LiteralPath $temp)) {
        Remove-Item -LiteralPath $temp -Recurse -Force
    }
}
