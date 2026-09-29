param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$GradleArguments
)

$ErrorActionPreference = 'Stop'
& agentctl run-gradle -- @GradleArguments
exit $LASTEXITCODE
