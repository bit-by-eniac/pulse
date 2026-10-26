# Builds Pulse for Windows and packages it as a .zip.
#
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1 [-Target x86_64-pc-windows-msvc]
#
# Output: dist\Pulse-<version>-windows-<arch>.zip
param([string]$Target = "x86_64-pc-windows-msvc")
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches.Groups[1].Value
$arch = if ($Target -like "aarch64*") { "arm64" } else { "x64" }

# Keep this machine's paths (user folder, checkout) out of the binary: Rust embeds
# source paths in panic messages. Encoded form, so paths with spaces survive.
$env:CARGO_ENCODED_RUSTFLAGS = "--remap-path-prefix=$env:USERPROFILE=~" + [char]0x1f + "--remap-path-prefix=$((Get-Location).Path)=."
rustup target add $Target | Out-Null
cargo build --release --target $Target
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$stage = "dist\stage-windows"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "target\$Target\release\pulse.exe" "$stage\Pulse.exe"
Copy-Item README.md $stage
$zip = "dist\Pulse-$version-windows-$arch.zip"
Remove-Item $zip -Force -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\*" -DestinationPath $zip
Remove-Item $stage -Recurse -Force
Write-Host "-> $zip"
