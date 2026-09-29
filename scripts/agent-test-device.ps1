param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$GradleArguments
)

$ErrorActionPreference = 'Stop'
& agentctl run-android-test -- @GradleArguments
exit $LASTEXITCODE
