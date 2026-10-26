# Builds Pulse for Windows and packages it as a .zip.
#
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1 [-Target x86_64-pc-windows-msvc] [-Full]
#
# -Full also makes a -full .zip with mpv, ffmpeg and QuickJS in tools\, from
# dist\tools-windows-<arch> (scripts/build-tools.sh windows-x64, in MSYS2).
# Output: dist\Pulse-<version>-windows-<arch>[-full].zip
param([string]$Target = "x86_64-pc-windows-msvc", [switch]$Full)
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
Write-Host "-> $zip"

if ($Full) {
  # In tools\ next to Pulse.exe, where it looks for them.
  $tools = "dist\tools-windows-$arch"
  foreach ($f in "mpv.exe", "ffmpeg.exe", "qjs.exe") {
    if (-not (Test-Path "$tools\$f")) { throw "missing $tools\${f}: run scripts/build-tools.sh windows-$arch in MSYS2 first" }
  }
  New-Item -ItemType Directory -Force "$stage\tools" | Out-Null
  Copy-Item "$tools\*.exe" "$stage\tools"
  Copy-Item "$tools\LICENSES" "$stage\tools\LICENSES" -Recurse
  $fullZip = "dist\Pulse-$version-windows-$arch-full.zip"
  Remove-Item $fullZip -Force -ErrorAction SilentlyContinue
  Compress-Archive -Path "$stage\*" -DestinationPath $fullZip
  Write-Host "-> $fullZip"
}
Remove-Item $stage -Recurse -Force
