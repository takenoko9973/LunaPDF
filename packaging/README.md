# Distribution packaging

Complete Windows build, installation, and update instructions are available in
[`docs/windows-distribution.md`](../docs/windows-distribution.md).

LunaPDF exposes PDF association metadata without changing the operating
system's default PDF application.

## Linux

Install `linux/lunapdf.desktop` as `lunapdf.desktop` in the distribution's
application directory alongside a `lunapdf` executable on `PATH`. The desktop
entry declares `application/pdf` and accepts all paths supplied by the file
manager through `%F`.

The package must not run `xdg-mime default` during installation. Users choose
LunaPDF as their default viewer through their desktop environment when wanted.

## Windows

Cross-compile the release executable first, then build both distribution forms
on Windows with Inno Setup 6 available on `PATH`:

```powershell
.\packaging\windows\build-distributions.ps1 `
  -ExecutablePath .\dist\lunapdf-release.exe `
  -TargetTriple x86_64-pc-windows-gnu `
  -OutputDirectory .\dist
```

The script reads the version from Cargo metadata and creates:

- `LunaPDF-Setup-<version>-x64.exe`
- `LunaPDF-Portable-<version>-x64.zip`

Run it from a clean Git working tree and pass the release executable built from
that checkout. The script refuses a dirty tree so `SOURCE-CODE.txt` can name an
exact source commit. It also refuses to replace either existing output file;
use an empty output directory for each build.

The installer writes only per-user files and registry entries. It registers
LunaPDF as an Open With and Default Apps candidate, but never changes the
protected `UserChoice` value. The portable ZIP performs no registration and
shares `%APPDATA%\LunaPDF` settings with the installed executable.

For a manually placed executable, run
`windows/register-pdf-association.ps1 -ExecutablePath C:\Path\To\LunaPDF.exe`.
The helper writes only the Open With candidate under `HKCU\Software\Classes`;
`-WhatIf` previews the operation before a writable registry handle is opened.
It is for initial registration only and refuses to overwrite existing LunaPDF
entries so a failed run can remove only the keys it created.
