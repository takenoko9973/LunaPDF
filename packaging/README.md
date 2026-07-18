# PDF association packaging

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

After installing `LunaPDF.exe`, explicitly run
`windows/register-pdf-association.ps1 -ExecutablePath C:\Path\To\LunaPDF.exe`.
The script writes only to `HKCU\Software\Classes`, registers LunaPDF in the
PDF “Open with” candidates, and leaves the protected default-app `UserChoice`
untouched. The `Player` multi-select model passes a file-manager selection to
one LunaPDF process, which opens paths as tabs up to the application limit.
`-WhatIf` previews the operation without opening the registry for writing.
