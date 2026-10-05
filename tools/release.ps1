<#
.SYNOPSIS
Builds better_search and publishes a GitHub release with the setup program and the
manifest the update check reads.

.DESCRIPTION
The release carries two files:
  latest.txt                 version, installer url and the installer's SHA-256
  better-search-setup.exe    the setup program (installs, upgrades and uninstalls)

The manifest url uses GitHub's "releases/latest", so the address inside latest.txt never
changes from one release to the next, and an installed copy finds each new release
without being repointed.

.PARAMETER Version
The version to publish, without a leading "v". It must already be the version in the
workspace Cargo.toml, because that is what the built programs report about themselves.

.PARAMETER DryRun
Build, stage the two files and print the manifest without publishing anything.

.EXAMPLE
.\tools\release.ps1 -Version 0.2.0

.EXAMPLE
.\tools\release.ps1 -Version 0.2.0 -DryRun
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Repo = 'devashish-guliya/better_search',
    [string]$Notes = '',
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$workspace = Get-Content -LiteralPath (Join-Path $root 'Cargo.toml') -Raw
$declared = [regex]::Match($workspace, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if ($declared -ne $Version) {
    throw "Cargo.toml declares version $declared; raise it to $Version before publishing."
}

# The setup program reports its own crate version in Apps & Features, so keep it equal
# to the release (the two manifests cannot inherit from each other: the installer is
# built outside the workspace).
$installerManifest = Join-Path $root 'tools\installer\Cargo.toml'
$installerText = Get-Content -LiteralPath $installerManifest -Raw
$installerVersion = [regex]::Match($installerText, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if ($installerVersion -ne $Version) {
    $updated = $installerText -replace '(?m)^version\s*=\s*"[^"]+"', "version = `"$Version`""
    Set-Content -LiteralPath $installerManifest -Value $updated -NoNewline
    Write-Output "Set the setup program's version to $Version (was $installerVersion)."
}

Push-Location $root
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    cargo build --release --manifest-path tools\installer\Cargo.toml
    if ($LASTEXITCODE -ne 0) { throw 'the setup program failed to build' }

    $setup = Join-Path $root 'tools\installer\target\release\better-search-setup.exe'
    if (-not (Test-Path -LiteralPath $setup)) { throw "missing $setup" }
    $hash = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLower()

    $stage = Join-Path $env:TEMP "better-search-release-$Version"
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    $stagedSetup = Join-Path $stage 'better-search-setup.exe'
    Copy-Item -LiteralPath $setup -Destination $stagedSetup -Force

    $manifest = @(
        "version=$Version"
        "url=https://github.com/$Repo/releases/latest/download/better-search-setup.exe"
        "sha256=$hash"
    ) -join "`n"
    $stagedManifest = Join-Path $stage 'latest.txt'
    Set-Content -LiteralPath $stagedManifest -Value $manifest -Encoding ascii

    Write-Output "Manifest staged at $stagedManifest"
    Write-Output $manifest
    if ($DryRun) {
        Write-Output "Dry run: nothing was published."
        return
    }
    if (-not $Notes) { $Notes = "better_search $Version" }
    gh release create "v$Version" --repo $Repo --title "better_search $Version" `
        --notes $Notes $stagedManifest $stagedSetup
    if ($LASTEXITCODE -ne 0) { throw 'gh release create failed' }
    Write-Output "Published v$Version to $Repo (sha256 $hash)"
}
finally {
    Pop-Location
}
