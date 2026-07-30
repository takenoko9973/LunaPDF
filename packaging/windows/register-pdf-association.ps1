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
    # WhatIf は書き込み可能なレジストリハンドルを開く前に停止させ、
    # プレビューで関連付けキーを部分的に作成しない。
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

    # Player と %* により、Explorer の複数選択を1つのプロセスへ渡す。
    # その後、LunaPDF 側で仕様上の20タブ上限を適用する。
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
    # OpenWithProgids は空の REG_NONE 値で候補を登録するため、
    # 保護された UserChoice の既定アプリケーションを変更しない。
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
