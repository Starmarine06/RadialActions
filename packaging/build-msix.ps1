# Builds dist\RadialActions-<version>.msix for upload to the Microsoft Store.
# The Store signs the package, so no certificate is needed for submission.
#
#   .\packaging\build-msix.ps1 -Name 12345Publisher.RadialActions -Publisher "CN=..." -PublisherDisplay "..."
#
# The identity values are on the app's "Product identity" page in Partner Center.
param(
    [string]$Name = "Starmarine06.RadialActions",
    [string]$Publisher = "CN=062E474A-3779-4DE6-95FB-7030E994447E",
    [string]$PublisherDisplay = "Starmarine06",
    [string]$DisplayName = "RadialActions"
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$version = ([regex]'(?m)^version = "([^"]+)"').Match((Get-Content Cargo.toml -Raw)).Groups[1].Value + ".0"

cargo build --release
if ($LASTEXITCODE) { throw "build failed" }

$sdk = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.*\x64\makeappx.exe" |
    Sort-Object { [version]$_.Directory.Parent.Name } | Select-Object -Last 1
if (-not $sdk) { throw "Windows SDK (makeappx.exe) not found" }
$tools = $sdk.Directory.FullName

$stage = Join-Path $root "target\msix"
if (Test-Path $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Force "$stage\Assets", "$root\dist" | Out-Null
Copy-Item target\release\radialactions.exe, LICENSE $stage
Copy-Item packaging\Assets\*.png "$stage\Assets"

$manifest = (Get-Content packaging\AppxManifest.xml -Raw).
    Replace("__NAME__", $Name).
    Replace("__PUBLISHER_DISPLAY__", [Security.SecurityElement]::Escape($PublisherDisplay)).
    Replace("__PUBLISHER__", [Security.SecurityElement]::Escape($Publisher)).
    Replace("__DISPLAY__", [Security.SecurityElement]::Escape($DisplayName)).
    Replace("__VERSION__", $version)
[IO.File]::WriteAllText("$stage\AppxManifest.xml", $manifest, (New-Object Text.UTF8Encoding $false))

# Index the scaled logos so Windows can pick the right size.
$config = Join-Path $root "target\priconfig.xml"
& "$tools\makepri.exe" createconfig /cf $config /dq en-US /o | Out-Null
if ($LASTEXITCODE) { throw "makepri createconfig failed" }
& "$tools\makepri.exe" new /pr $stage /cf $config /mn "$stage\AppxManifest.xml" /of "$stage\resources.pri" /o | Out-Null
if ($LASTEXITCODE) { throw "makepri failed" }

$out = Join-Path $root "dist\RadialActions-$version.msix"
& "$tools\makeappx.exe" pack /d $stage /p $out /o | Out-Null
if ($LASTEXITCODE) { throw "makeappx failed" }
Write-Host "Built $out"
