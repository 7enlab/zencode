# Install ZenCode CLI

ZenCode has no published binary release yet, so the one-command
install builds the CLI from source. You need Git and a Rust toolchain
(`cargo`) installed first. The build can take a few minutes.

## One command

Inspect the script before running it if you want to. The scripts only
clone this repo, build `zen-cli` in release mode, and copy the
resulting binary to the install directory. They never use sudo or
administrator rights.

Linux and macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/7enlabs/zencode/main/install.sh | sh
```

Windows, in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/7enlabs/zencode/main/install.ps1 | iex"
```

Safer alternative on Windows, download first and read it:

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/7enlabs/zencode/main/install.ps1 -OutFile install.ps1
.\install.ps1
```

## Where the binary goes

- Linux and macOS: `~/.local/bin/zen`. If that directory is not on
  your PATH, the script prints the export line to add.
- Windows: `%USERPROFILE%\.local\bin\zen.exe`. The script adds that
  directory to the user PATH if it is not there yet. Open a new
  terminal after installing.

## Update

Rerun the same one-command install. The script fast-forwards the
existing checkout and rebuilds.

## Manual install

```sh
git clone https://github.com/7enlabs/zencode.git
cd zencode
cargo build --release -p zen-cli
cp target/release/zen ~/.local/bin/zen
```

## Uninstall

Remove the binary and the checkout:

```sh
rm ~/.local/bin/zen
rm -rf ~/.local/share/zencode
```

On Windows, remove `%USERPROFILE%\.local\bin\zen.exe` and
`%USERPROFILE%\.local\share\zencode`, and remove the install
directory from the user PATH if you no longer want it there.

## Troubleshooting

- `cargo not found`: install Rust first, for example from
  https://rustup.rs, then rerun the install.
- `git not found`: install Git first, then rerun the install.
- The build needs network access once, for the Rust crate registry.
  After that, the binary runs against any OpenAI-compatible endpoint,
  including fully offline local models. See the README for providers.
