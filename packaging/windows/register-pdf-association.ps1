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

$applicationCreated = $false
$documentCreated = $false
$openWithValueCreated = $false
$currentUserSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$mutexName = "Local\LunaPDF.PdfAssociation.$currentUserSid"
$registrationMutex = [System.Threading.Mutex]::new($false, $mutexName)
$mutexAcquired = $false

try {
    try {
        # 事前検査と書込みを同一ユーザー内で直列化し、並行実行が互いの
        # 専用キーを作成済みと誤認してrollbackする競合を防ぐ。
        $mutexAcquired = $registrationMutex.WaitOne(0)
    }
    catch [System.Threading.AbandonedMutexException] {
        # 直前の所有プロセスが異常終了した場合、Windowsはこのスレッドへ
        # 所有権を渡してから例外を通知するため、安全に登録を再開できる。
        $mutexAcquired = $true
    }
    if (-not $mutexAcquired) {
        throw 'Another LunaPDF PDF association registration is already in progress.'
    }

    # 既存登録を上書きすると失敗時に元状態を復元できないため、手動ヘルパーは
    # 未登録状態だけを扱う。更新とアンインストールはInno Setupへ委ねる。
    foreach ($ownedSubkey in @('Applications\LunaPDF.exe', 'LunaPDF.Document.1')) {
        $existingKey = $classes.OpenSubKey($ownedSubkey, $false)
        if ($null -ne $existingKey) {
            $existingKey.Dispose()
            throw "LunaPDF registration already exists: HKCU:\$classesRoot\$ownedSubkey"
        }
    }
    $existingOpenWith = $classes.OpenSubKey('.pdf\OpenWithProgids', $false)
    if ($null -ne $existingOpenWith) {
        $openWithValueExists = $existingOpenWith.GetValueNames() -contains 'LunaPDF.Document.1'
        $existingOpenWith.Dispose()
        if ($openWithValueExists) {
            throw 'LunaPDF.Document.1 is already registered as a PDF Open With candidate.'
        }
    }

    $application = $classes.CreateSubKey('Applications\LunaPDF.exe')
    $applicationCreated = $true
    $application.SetValue('FriendlyAppName', 'LunaPDF')
    $application.Dispose()

    # Player と %* により、Explorer の複数選択を1つのプロセスへ渡す。
    # %1 と %* が先頭パスを重複させた場合は、通常のオープン経路が同一パスを1タブに正規化する。
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
    $documentCreated = $true
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
    $openWithValueCreated = $true
    $pdfOpenWith.Dispose()
}
catch {
    $registrationError = $_
    try {
        # 逆順rollbackは、上の事前確認後にこの実行が新規作成した
        # LunaPDF専用キーと値だけを対象にする。
        if ($openWithValueCreated) {
            $pdfOpenWith = $classes.OpenSubKey('.pdf\OpenWithProgids', $true)
            if ($null -ne $pdfOpenWith) {
                $pdfOpenWith.DeleteValue('LunaPDF.Document.1', $false)
                $pdfOpenWith.Dispose()
            }
        }
        if ($documentCreated) {
            $classes.DeleteSubKeyTree('LunaPDF.Document.1', $false)
        }
        if ($applicationCreated) {
            $classes.DeleteSubKeyTree('Applications\LunaPDF.exe', $false)
        }
    }
    catch {
        throw [System.AggregateException]::new(
            'LunaPDF registration and its rollback both failed.',
            @($registrationError.Exception, $_.Exception)
        )
    }
    throw $registrationError
}
finally {
    $classes.Dispose()
    if ($mutexAcquired) {
        $registrationMutex.ReleaseMutex()
    }
    $registrationMutex.Dispose()
}

Write-Output 'LunaPDF was registered as a PDF Open With candidate for the current user.'
