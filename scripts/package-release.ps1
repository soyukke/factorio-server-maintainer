param(
    [string]$Version = "dev",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$DistDir = Join-Path $RepoRoot "dist"
$PackageName = "factorio-server-maintainer-$Version-windows-x64"
$PackageDir = Join-Path $DistDir $PackageName
$ZipPath = Join-Path $DistDir "$PackageName.zip"

if (-not $SkipBuild) {
    cargo build --release -p gui-slint -p ctrlc-helper
}

Remove-Item -Recurse -Force -LiteralPath $PackageDir -ErrorAction SilentlyContinue
Remove-Item -Force -LiteralPath $ZipPath -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $PackageDir | Out-Null

Copy-Item -Force -Path (Join-Path $RepoRoot "target\release\factorio-server-manager.exe") -Destination $PackageDir
Copy-Item -Force -Path (Join-Path $RepoRoot "target\release\ctrlc-helper.exe") -Destination $PackageDir
Copy-Item -Force -Path (Join-Path $RepoRoot "README.md") -Destination $PackageDir
Copy-Item -Recurse -Force -Path (Join-Path $RepoRoot "docs") -Destination $PackageDir
Copy-Item -Force -Path (Join-Path $RepoRoot "docs\release-start.ja.md") -Destination (Join-Path $PackageDir "START-HERE.ja.md")
Copy-Item -Force -Path (Join-Path $RepoRoot "docs\release-start.en.md") -Destination (Join-Path $PackageDir "START-HERE.en.md")

Compress-Archive -Path (Join-Path $PackageDir "*") -DestinationPath $ZipPath -CompressionLevel Optimal

Get-FileHash -Algorithm SHA256 -LiteralPath $ZipPath |
    ForEach-Object { "$($_.Hash)  $(Split-Path -Leaf $ZipPath)" } |
    Set-Content -Path "$ZipPath.sha256" -Encoding ascii

Write-Host "Created:"
Write-Host "  $ZipPath"
Write-Host "  $ZipPath.sha256"
