param(
    [string]$Target = "",
    [string]$ArchLabel = ""
)

$ErrorActionPreference = "Stop"

$Root = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $Root
try {
    if ([string]::IsNullOrWhiteSpace($ArchLabel)) {
        if ($Target -like "aarch64*") {
            $ArchLabel = "arm64"
        } elseif ($env:PROCESSOR_ARCHITECTURE -eq "ARM64" -and [string]::IsNullOrWhiteSpace($Target)) {
            $ArchLabel = "arm64"
        } else {
            $ArchLabel = "x86_64"
        }
    }

    $BuildArgs = @("build", "--release", "-p", "smartkey-cli", "--bin", "smartkey", "-p", "smartkey-tsf")
    if (-not [string]::IsNullOrWhiteSpace($Target)) {
        $BuildArgs += @("--target", $Target)
        $ReleaseDir = Join-Path $Root "target\$Target\release"
    } else {
        $ReleaseDir = Join-Path $Root "target\release"
    }

    Write-Host "Building SmartKey Windows release ($ArchLabel)..."
    & cargo @BuildArgs
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    $StageDir = Join-Path $Root "dist\smartkey-windows-$ArchLabel"
    $ZipPath = Join-Path $Root "dist\smartkey-windows-$ArchLabel.zip"

    if (Test-Path $StageDir) {
        Remove-Item -Recurse -Force $StageDir
    }
    New-Item -ItemType Directory -Force -Path $StageDir | Out-Null

    Copy-Item (Join-Path $ReleaseDir "smartkey.exe") (Join-Path $StageDir "smartkey.exe")
    Copy-Item (Join-Path $ReleaseDir "smartkey_tsf.dll") (Join-Path $StageDir "smartkey_tsf.dll")
    Copy-Item (Join-Path $Root "packaging\windows\install.ps1") (Join-Path $StageDir "install.ps1")
    Copy-Item (Join-Path $Root "packaging\windows\uninstall.ps1") (Join-Path $StageDir "uninstall.ps1")
    Copy-Item (Join-Path $Root "packaging\windows\README.md") (Join-Path $StageDir "README.md")

    if (Test-Path $ZipPath) {
        Remove-Item -Force $ZipPath
    }
    Compress-Archive -Path "$StageDir\*" -DestinationPath $ZipPath -Force
    Write-Host "Packaged: $ZipPath"
} finally {
    Pop-Location
}
