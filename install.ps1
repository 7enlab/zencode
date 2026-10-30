# Install ZenCode CLI from source on Windows.
#
# One command (inspect the script first if you want to):
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/7enlabs/zencode/main/install.ps1 | iex"
#
# Safer alternative, download first and read it before running:
#   Invoke-WebRequest https://raw.githubusercontent.com/7enlabs/zencode/main/install.ps1 -OutFile install.ps1
#   .\install.ps1
#
# What this script does, and nothing else:
#   1. Clones the repo, or fast-forwards it if it already exists.
#   2. Builds the CLI crate in release mode with cargo.
#   3. Copies the resulting `zen.exe` binary to the install directory.
#   4. Adds the install directory to the user PATH if it is not there yet.
# It never requires administrator rights and never installs a Rust
# toolchain for you. If cargo is missing, it stops and tells you
# how to install Rust.
#
# Environment overrides, all optional:
#   ZENCODE_REPO        Git URL to clone.
#   ZENCODE_BRANCH      Branch to build (default: main).
#   ZENCODE_SRC_DIR     Where the repo is cloned.
#   ZENCODE_INSTALL_DIR Where the `zen.exe` binary is copied.
$ErrorActionPreference = 'Stop'

$Repo = if ($env:ZENCODE_REPO) { $env:ZENCODE_REPO } else { 'https://github.com/7enlabs/zencode.git' }
$Branch = if ($env:ZENCODE_BRANCH) { $env:ZENCODE_BRANCH } else { 'main' }
$SrcDir = if ($env:ZENCODE_SRC_DIR) { $env:ZENCODE_SRC_DIR } else { Join-Path $env:USERPROFILE '.local\share\zencode' }
$InstallDir = if ($env:ZENCODE_INSTALL_DIR) { $env:ZENCODE_INSTALL_DIR } else { Join-Path $env:USERPROFILE '.local\bin' }

if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    throw 'git not found. Install Git for Windows first, then rerun this script.'
}

$Cargo = $null
if (Get-Command cargo -ErrorAction SilentlyContinue) {
    $Cargo = 'cargo'
} elseif (Test-Path (Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe')) {
    $Cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
} else {
    throw 'cargo not found. Install Rust first, for example from https://rustup.rs, then rerun this script.'
}

if (Test-Path (Join-Path $SrcDir '.git')) {
    Write-Host "Updating existing checkout in $SrcDir"
    git -C $SrcDir fetch origin $Branch
    git -C $SrcDir checkout $Branch
    git -C $SrcDir merge --ff-only "origin/$Branch"
} else {
    Write-Host "Cloning $Repo ($Branch) into $SrcDir"
    git clone --branch $Branch --depth 1 $Repo $SrcDir
}

Write-Host 'Building zen-cli in release mode (this can take a few minutes)'
& $Cargo build --release -p zen-cli --manifest-path (Join-Path $SrcDir 'Cargo.toml')

$Bin = Join-Path $SrcDir 'target\release\zen.exe'
if (-not (Test-Path $Bin)) {
    throw "Build finished but $Bin is missing."
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item $Bin (Join-Path $InstallDir 'zen.exe') -Force

$UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$Parts = $UserPath -split ';' | Where-Object { $_ -ne '' }
if ($Parts -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable('Path', "$UserPath;$InstallDir", 'User')
    $env:Path = "$env:Path;$InstallDir"
    Write-Host "Added $InstallDir to the user PATH. Open a new terminal to use it."
}

Write-Host "Installed zen to $(Join-Path $InstallDir 'zen.exe')"
try {
    & (Join-Path $InstallDir 'zen.exe') --help | Out-Null
} catch {
    Write-Warning 'The installed binary did not respond to --help; run it directly to check it.'
}
Write-Host 'Run: zen'
