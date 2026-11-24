param(
    [switch]$User
)

$ErrorActionPreference = "Stop"

$Cli = Join-Path $PSScriptRoot "smartkey.exe"
$Dll = Join-Path $PSScriptRoot "smartkey_tsf.dll"

if (-not (Test-Path $Cli)) {
    Write-Error "smartkey.exe not found next to install.ps1"
    exit 1
}
if (-not (Test-Path $Dll)) {
    Write-Error "smartkey_tsf.dll not found next to install.ps1"
    exit 1
}

$CliArgs = @("install", "--dll", $Dll)
if ($User) {
    $CliArgs += "--user"
}

& $Cli @CliArgs
exit $LASTEXITCODE
