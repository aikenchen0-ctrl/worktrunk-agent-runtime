param(
    [ValidateSet('json', 'powershell', 'shell')]
    [string]$Format = 'powershell'
)

$ErrorActionPreference = 'Stop'
if ($Format -eq 'powershell') {
    $output = & agentctl env --format $Format
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    foreach ($line in $output) {
        Invoke-Expression $line
    }
} else {
    & agentctl env --format $Format
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}
