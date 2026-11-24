param(
    [switch]$User
)

$ErrorActionPreference = "Stop"

$InstalledCli = "$env:ProgramFiles\SmartKey\smartkey.exe"
$UserCli = "$env:LOCALAPPDATA\Programs\SmartKey\smartkey.exe"
$LocalCli = Join-Path $PSScriptRoot "smartkey.exe"

if (Test-Path $LocalCli) {
    $Cli = $LocalCli
} elseif (Test-Path $InstalledCli) {
    $Cli = $InstalledCli
} elseif (Test-Path $UserCli) {
    $Cli = $UserCli
} else {
    Write-Error "smartkey.exe not found"
    exit 1
}

$CliArgs = @("uninstall")
if ($User) {
    $CliArgs += "--user"
}

& $Cli @CliArgs
exit $LASTEXITCODE
