param(
    [Parameter(Mandatory = $true)]
    [string]$EventFile,
    [Parameter(Mandatory = $true)]
    [string]$Consumer,
    [Parameter(Mandatory = $true)]
    [string]$GradleTask,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[A-Za-z][A-Za-z0-9_.-]*$')]
    [string]$CandidateProperty,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^(:|(:[A-Za-z0-9_.-]+)+)$')]
    [string]$ConsumerProject,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[A-Za-z][A-Za-z0-9_.-]*$')]
    [string]$Configuration,
    [switch]$AllowLoopbackHttp,
    [switch]$RerunTasks,
    [string]$AgentRuntimeRoot = $env:AGENT_RUNTIME_ROOT,
    [string]$AgentCtl = 'agentctl'
)

$ErrorActionPreference = 'Stop'
$managedVariables = @(
    'AGENT_RUNTIME_ROOT', 'AGENT_CANDIDATE_VERSION', 'AGENT_CANDIDATE_COORDINATE',
    'AGENT_CANDIDATE_CONSUMER_PROJECT', 'AGENT_CANDIDATE_CONFIGURATION',
    'AGENT_CANDIDATE_EXTENSION', 'AGENT_CANDIDATE_TEST_TASK', 'AGENT_ARTIFACT_SHA256',
    'AGENT_VALIDATION_ATTEMPT_ID', 'AGENT_VALIDATION_CONSUMER'
)
$savedEnvironment = @{}
foreach ($name in $managedVariables) { $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
$downloadDirectory = $null
$download = $null
$validationActive = $false
try {
$event = Get-Content -Raw -LiteralPath $EventFile | ConvertFrom-Json
$eventId = [string]$event.event_id
$candidate = [string]$event.candidate_version
$artifact = $event.artifact
$env:AGENT_RUNTIME_ROOT = $AgentRuntimeRoot
$verificationScript = Join-Path $PSScriptRoot 'verify-candidate.gradle'
if (-not (Test-Path -LiteralPath $verificationScript)) {
    throw "找不到 Gradle 候选制品校验脚本：$verificationScript"
}

if ([string]::IsNullOrWhiteSpace($eventId) -or [string]::IsNullOrWhiteSpace($candidate)) {
    throw '候选事件缺少 event_id 或 candidate_version'
}
if ($eventId -cnotmatch '^[A-Za-z0-9_-]{1,128}$') {
    throw '候选事件 ID 包含非法字符'
}
$taskPrefix = if ($ConsumerProject -eq ':') { ':' } else { "$($ConsumerProject):" }
if ($GradleTask -cnotmatch ('^' + [regex]::Escape($taskPrefix) + '[A-Za-z][A-Za-z0-9_]*$')) {
    throw 'GradleTask 必须是消费者项目内的单个完整任务路径'
}
if (@($event.affected_consumers) -cnotcontains $Consumer) {
    throw "消费者 $Consumer 不在候选事件的受影响列表中"
}
if ($null -eq $artifact -or [string]::IsNullOrWhiteSpace([string]$artifact.url) -or [string]::IsNullOrWhiteSpace([string]$artifact.sha256) -or [string]::IsNullOrWhiteSpace([string]$artifact.coordinate)) {
    throw '候选事件缺少完整制品证据：url、sha256、coordinate'
}
if ([string]$artifact.coordinate -notmatch '^[^:]+:[^:]+:[^:]+$' -or ([string]$artifact.coordinate).Split(':')[2] -ne $candidate) {
    throw '候选制品坐标中的版本与事件 candidate_version 不一致'
}
$artifactUri = [uri][string]$artifact.url
if ($artifactUri.Scheme -ne 'https') {
    if (-not $AllowLoopbackHttp -or $artifactUri.Scheme -ne 'http' -or $artifactUri.Host -notin @('127.0.0.1', '[::1]', '::1')) {
        throw '候选制品 URL 必须使用 HTTPS；本机实测可显式启用 AllowLoopbackHttp'
    }
    Write-Warning '当前使用本机 HTTP 制品测试，不代表远端可信发布验证'
}
$extension = [System.IO.Path]::GetExtension($artifactUri.AbsolutePath).TrimStart('.').ToLowerInvariant()
if ($extension -notin @('aar', 'jar')) { throw '候选制品必须是 AAR 或 JAR' }
if ([string]$artifact.sha256 -notmatch '^[a-fA-F0-9]{64}$') { throw '候选制品 SHA-256 无效' }
$env:AGENT_CANDIDATE_VERSION = $candidate
$env:AGENT_CANDIDATE_COORDINATE = [string]$artifact.coordinate
$env:AGENT_CANDIDATE_CONSUMER_PROJECT = $ConsumerProject
$env:AGENT_CANDIDATE_CONFIGURATION = $Configuration
$env:AGENT_CANDIDATE_EXTENSION = $extension
$env:AGENT_CANDIDATE_TEST_TASK = $GradleTask.Substring($taskPrefix.Length)
$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$downloadDirectory = Join-Path $tempRoot ([guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $downloadDirectory | Out-Null
$download = Join-Path $downloadDirectory "candidate.$extension"
Invoke-WebRequest -Uri $artifactUri -OutFile $download
$actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $download).Hash.ToLowerInvariant()
if ($actual -ne ([string]$artifact.sha256).ToLowerInvariant()) {
    throw "候选制品 SHA-256 校验失败：期望 $($artifact.sha256)，实际 $actual"
}
$env:AGENT_ARTIFACT_SHA256 = $actual

& $AgentCtl event-import --file $EventFile
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$attemptJson = & $AgentCtl event-ack --event $eventId --consumer $Consumer --status validation_started --format json
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$attempt = $attemptJson | ConvertFrom-Json
$validationActive = $true
$env:AGENT_VALIDATION_ATTEMPT_ID = [string]$attempt.attempt_id
$env:AGENT_VALIDATION_CONSUMER = $Consumer
$gradleArguments = @("-I$verificationScript", $GradleTask, "-P$CandidateProperty=$candidate")
if ($RerunTasks) { $gradleArguments += '--rerun-tasks' }
& $AgentCtl run-gradle --event $eventId -- @gradleArguments
$gradleExit = $LASTEXITCODE
if ($gradleExit -eq 0) {
    $runtimeEnvironment = (& $AgentCtl env --format json) | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $receiptPath = Join-Path $runtimeEnvironment.AGENT_RUNTIME_ROOT "agents\$($runtimeEnvironment.AGENT_ID)\$($runtimeEnvironment.AGENT_RUNTIME_ID)\build-result.json"
    if (-not (Test-Path -LiteralPath $receiptPath)) { throw 'Gradle 成功但当前 Agent 没有构建收据' }
    $receipt = Get-Content -Raw -LiteralPath $receiptPath | ConvertFrom-Json
    if ($receipt.event_id -ne $eventId -or -not $receipt.success -or $receipt.artifact_sha256 -ne $actual) {
        throw '当前 Agent 的构建收据与候选事件不匹配'
    }
    & $AgentCtl event-ack --event $eventId --consumer $Consumer --status passed --receipt $receiptPath
    if ($LASTEXITCODE -ne 0) {
        $ackExit = $LASTEXITCODE
        & $AgentCtl event-ack --event $eventId --consumer $Consumer --status failed --message 'Gradle 退出成功，但验证证据不完整或已失效'
        $validationActive = $false
        exit $ackExit
    }
} else {
    & $AgentCtl event-ack --event $eventId --consumer $Consumer --status failed --message "Gradle 退出码 $gradleExit"
}
$validationActive = $false
exit $gradleExit
} catch {
    if ($validationActive) {
        & $AgentCtl event-ack --event $eventId --consumer $Consumer --status failed --message '候选验证适配器失败，请查看本次日志'
    }
    throw
} finally {
    # 仅删除本次创建的下载文件和空目录，不进行递归清理。
    if ($download -and (Test-Path -LiteralPath $download)) { Remove-Item -LiteralPath $download -Force }
    if ($downloadDirectory -and (Test-Path -LiteralPath $downloadDirectory)) { Remove-Item -LiteralPath $downloadDirectory }
    foreach ($name in $managedVariables) { [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process') }
}
