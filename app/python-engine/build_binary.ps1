# Builds the OPTIONAL Python fallback engine on Windows.
# See build_binary.sh / README.md for what this is and when you need it.
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

# Guard: markitdown[all]'s native deps lack wheels for the newest Python, so a
# too-new interpreter makes pip build from source and hang. Require 3.10–3.13
# (3.12 matches the GitHub workflow). Set $env:PYTHON to choose a specific one.
$py = if ($env:PYTHON) { $env:PYTHON } else { "python" }
$ver = & $py -c "import sys; print('%d.%d' % sys.version_info[:2])"
$parts = $ver.Split('.')
if ([int]$parts[0] -ne 3 -or [int]$parts[1] -lt 10 -or [int]$parts[1] -gt 13) {
    Write-Error "Python $ver is not supported (need 3.10-3.13; 3.12 recommended). Set `$env:PYTHON to a supported interpreter."
    exit 1
}
Write-Host "==> using $py ($ver)"

& $py -m venv .venv
& .venv\Scripts\Activate.ps1

pip install --quiet --upgrade pip
# markitdown[all] already pulls youtube-transcript-api via its
# youtube-transcription extra; install it explicitly too so the YouTube
# transcript fallback can't silently disappear if the extra is renamed.
pip install --quiet pyinstaller "markitdown[all]" youtube-transcript-api

@"
from markitdown.__main__ import main

if __name__ == "__main__":
    main()
"@ | Set-Content -Encoding utf8 _entry.py

# onedir (default): a folder that extracts once — fast startup on repeated use.
# Set $env:BUILD_MODE = "onefile" for a single portable .exe (slower cold start).
$mode = if ($env:BUILD_MODE) { "--$($env:BUILD_MODE)" } else { "--onedir" }
pyinstaller $mode --name markitdown-py `
    --collect-all magika `
    --collect-data charset_normalizer `
    --copy-metadata markitdown `
    _entry.py

$bin = if ($env:BUILD_MODE -eq "onefile") {
    "$PSScriptRoot\dist\markitdown-py.exe"
} else {
    "$PSScriptRoot\dist\markitdown-py\markitdown-py.exe"
}
Write-Host ""
Write-Host "Built: $bin"
Write-Host "Enable it with:  `$env:MARKITDOWN_PY_BIN = `"$bin`""
