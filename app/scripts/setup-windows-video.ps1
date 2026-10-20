# Project-local FFmpeg development/runtime files and libclang for bindgen.
# Run before Cargo on Windows; no global installs or PATH changes are made.
param(
    [string]$ToolsDirectory = (Join-Path $PSScriptRoot '../../target/windows-tools'),
    [switch]$GitHubActions
)
$ErrorActionPreference = 'Stop'
$tools = [System.IO.Path]::GetFullPath($ToolsDirectory)
New-Item -ItemType Directory -Path $tools -Force | Out-Null

function Get-VerifiedArchive([string]$Url, [string]$Name, [string]$Sha256) {
    $archive = Join-Path $tools $Name
    if (!(Test-Path -LiteralPath $archive) -or (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $Sha256) {
        & curl.exe --fail --location --retry 3 --output $archive $Url
        if ($LASTEXITCODE -ne 0) { throw "Download failed: $Name" }
    }
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $Sha256) {
        throw "SHA-256 mismatch: $Name"
    }
    return $archive
}

$sdk = Join-Path $tools 'ffmpeg-9.0.2-full_build-shared'
if (!(Test-Path -LiteralPath (Join-Path $sdk 'lib/avcodec.lib'))) {
    $archive = Get-VerifiedArchive `
        'https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-full_build-shared.7z' `
        'ffmpeg-9.0.2-full_build-shared.7z' `
        '4d2060a8b34a940aa47d785142055bb92a63053781e55f2ace4546edd519a8f5'
    & tar.exe -xf $archive -C $tools
    if ($LASTEXITCODE -ne 0) { throw 'FFmpeg extraction failed' }
}
$clang = Join-Path $tools 'libclang-18.1.1/libclang-18.1.1.data/platlib/clang/native'
if (!(Test-Path -LiteralPath (Join-Path $clang 'libclang.dll'))) {
    $archive = Get-VerifiedArchive `
        'https://files.pythonhosted.org/packages/0b/2d/3f480b1e1d31eb3d6de5e3ef641954e5c67430d5ac93b7fa7e07589576c7/libclang-18.1.1-py2.py3-none-win_amd64.whl' `
        'libclang-18.1.1.zip' `
        '4dd2d3b82fab35e2bf9ca717d7b63ac990a3519c7e312f19fa8e86dcc712f7fb'
    Expand-Archive -LiteralPath $archive -DestinationPath (Join-Path $tools 'libclang-18.1.1') -Force
}
$env:FFMPEG_DIR = $sdk
$env:LIBCLANG_PATH = $clang
$env:Path = (Join-Path $sdk 'bin') + [System.IO.Path]::PathSeparator + $env:Path
if ($GitHubActions) {
    Add-Content -LiteralPath $env:GITHUB_ENV -Value "FFMPEG_DIR=$sdk"
    Add-Content -LiteralPath $env:GITHUB_ENV -Value "LIBCLANG_PATH=$clang"
    Add-Content -LiteralPath $env:GITHUB_PATH -Value (Join-Path $sdk 'bin')
}
Write-Host "FFmpeg SDK: $sdk"
Write-Host "libclang: $clang"
