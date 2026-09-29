#Requires -Version 5.1
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('pre-start', 'pre-merge', 'pre-remove', 'post-remove')]
    [string]$Event,
    [string]$AgentCtl = (Join-Path $PSScriptRoot '..\bin\agentctl.exe')
)

$ErrorActionPreference = 'Stop'
try {
    if (-not (Test-Path -LiteralPath $AgentCtl -PathType Leaf)) {
        throw '未找到 agentctl，请先完成安装或显式指定 AgentCtl。'
    }
    switch ($Event) {
        'pre-start' { & $AgentCtl prepare --lifecycle }
        'pre-merge' { & $AgentCtl run-gradle --lifecycle -- check }
        'pre-remove' { & $AgentCtl cleanup --worktree }
        'post-remove' {
            # 只把 JSON 路径作为一个参数传递，绝不将分支名拼接成可执行脚本。
            if (-not [Console]::IsInputRedirected) { throw 'post-remove 必须接收 Worktrunk JSON 标准输入。' }
            $context = [Console]::In.ReadToEnd() | ConvertFrom-Json
            $path = $context.worktree_path
            if ($path -isnot [string] -or -not [IO.Path]::IsPathRooted($path) -or $path -notmatch '^(?:[A-Za-z]:[\\/]|\\\\)') {
                throw 'Worktrunk 上下文缺少绝对 worktree_path，已拒绝清理。'
            }
            & $AgentCtl cleanup --worktree --worktree-path $path
        }
    }
    exit $LASTEXITCODE
} catch {
    [Console]::Error.WriteLine("Worktrunk Hook 失败：$($_.Exception.Message)")
    exit 1
}
