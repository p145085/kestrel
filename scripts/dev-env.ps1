# Set up a shell for building and running the media parts of Kestrel.
#
#   . .\scripts\dev-env.ps1
#
# GStreamer's winget package does not put itself on PATH, so both the build
# (pkg-config, headers, import libraries) and the run (DLLs, plugins) need to
# be pointed at it.

$candidates = @(
    "$env:LOCALAPPDATA\Programs\gstreamer\1.0\msvc_x86_64",
    "C:\gstreamer\1.0\msvc_x86_64",
    "$env:ProgramFiles\gstreamer\1.0\msvc_x86_64"
)
$root = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1

if (-not $root) {
    Write-Error "GStreamer not found. Install it with: winget install gstreamerproject.gstreamer"
    return
}

$env:GSTREAMER_1_0_ROOT_MSVC_X86_64 = $root
$env:PKG_CONFIG_PATH = "$root\lib\pkgconfig"
if ($env:Path -notlike "*$root\bin*") { $env:Path = "$root\bin;$env:Path" }

Write-Output "GStreamer $(& "$root\bin\pkg-config.exe" --modversion gstreamer-1.0) at $root"
