# Set up a shell for building and running the media parts of Kestrel, and the
# graphical client.
#
#   . .\scripts\dev-env.ps1
#
# GStreamer's winget package does not put itself on PATH, so both the build
# (pkg-config, headers, import libraries) and the run (DLLs, plugins) need to
# be pointed at it.
#
# The same package carries GTK 4, so kestrel-ui needs this too -- and needs it
# to RUN, not merely to build: its DLLs are resolved before main starts, so
# without this it exits with STATUS_DLL_NOT_FOUND and no message at all.

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

Write-Output "GStreamer $(& "$root\bin\pkg-config.exe" --modversion gstreamer-1.0), GTK $(& "$root\bin\pkg-config.exe" --modversion gtk4) at $root"
