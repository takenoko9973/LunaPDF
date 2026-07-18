[CmdletBinding(SupportsShouldProcess)]
param(
    [Parameter(Mandatory)]
    [string] $ExecutablePath
)

$resolvedExecutable = (Resolve-Path -LiteralPath $ExecutablePath -ErrorAction Stop).Path
if (-not (Test-Path -LiteralPath $resolvedExecutable -PathType Leaf)) {
    throw 'ExecutablePath must identify an installed executable file.'
}
if ([System.IO.Path]::GetFileName($resolvedExecutable) -ine 'LunaPDF.exe') {
    throw 'ExecutablePath must point to LunaPDF.exe.'
}

$classesRoot = 'Software\Classes'

if (-not $PSCmdlet.ShouldProcess('the current user registry', 'Register LunaPDF as a PDF Open With candidate')) {
    # WhatIf must stop before opening a writable registry handle so the preview
    # cannot leave partially-created association keys.
    return
}

$classes = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($classesRoot, $true)
if ($null -eq $classes) {
    throw "Cannot open HKCU:\$classesRoot for writing."
}

try {
    $application = $classes.CreateSubKey('Applications\LunaPDF.exe')
    $application.SetValue('FriendlyAppName', 'LunaPDF')
    $application.Dispose()

    # Player plus %* asks Explorer to pass a multi-selection to one process;
    # LunaPDF then applies its own documented twenty-tab limit.
    $applicationOpen = $classes.CreateSubKey('Applications\LunaPDF.exe\shell\open')
    $applicationOpen.SetValue('MultiSelectModel', 'Player')
    $applicationOpen.Dispose()

    $applicationCommand = $classes.CreateSubKey('Applications\LunaPDF.exe\shell\open\command')
    $applicationCommand.SetValue('', ('"{0}" "%1" %*' -f $resolvedExecutable))
    $applicationCommand.Dispose()

    $supportedTypes = $classes.CreateSubKey('Applications\LunaPDF.exe\SupportedTypes')
    $supportedTypes.SetValue('.pdf', '')
    $supportedTypes.Dispose()

    $document = $classes.CreateSubKey('LunaPDF.Document.1')
    $document.SetValue('', 'PDF document')
    $document.Dispose()

    $documentOpen = $classes.CreateSubKey('LunaPDF.Document.1\shell\open')
    $documentOpen.SetValue('MultiSelectModel', 'Player')
    $documentOpen.Dispose()

    $documentCommand = $classes.CreateSubKey('LunaPDF.Document.1\shell\open\command')
    $documentCommand.SetValue('', ('"{0}" "%1" %*' -f $resolvedExecutable))
    $documentCommand.Dispose()

    $pdfOpenWith = $classes.CreateSubKey('.pdf\OpenWithProgids')
    # OpenWithProgids uses an empty REG_NONE value. It advertises a candidate
    # without changing the protected UserChoice default application.
    $pdfOpenWith.SetValue(
        'LunaPDF.Document.1',
        [byte[]] @(),
        [Microsoft.Win32.RegistryValueKind]::None
    )
    $pdfOpenWith.Dispose()
}
finally {
    $classes.Dispose()
}

Write-Output 'LunaPDF was registered as a PDF Open With candidate for the current user.'
