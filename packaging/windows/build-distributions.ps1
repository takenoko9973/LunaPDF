<#
.SYNOPSIS
Builds the per-user installer and portable ZIP from one Windows x64 release executable.

.DESCRIPTION
Reads the package version from Cargo metadata, assembles license/source material, creates the
portable ZIP, and compiles LunaPDF.iss with Inno Setup. The input executable is copied as
LunaPDF.exe; it is never modified in place.

.PARAMETER ExecutablePath
Path to the Windows x64 release executable built from the current clean checkout.

.PARAMETER TargetTriple
Rust target triple used to build ExecutablePath. Windows dependency licenses are resolved for
this exact target.

.PARAMETER OutputDirectory
Directory that receives the installer and portable ZIP. Defaults to the repository dist folder.

.PARAMETER InnoCompilerPath
Optional path to ISCC.exe. If omitted, ISCC.exe must be available on PATH.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $ExecutablePath,

    [Parameter(Mandatory)]
    [ValidateSet('x86_64-pc-windows-msvc', 'x86_64-pc-windows-gnu')]
    [string] $TargetTriple,

    [string] $OutputDirectory,

    [string] $InnoCompilerPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$resolvedExecutable = (Resolve-Path -LiteralPath $ExecutablePath -ErrorAction Stop).Path
if (-not (Test-Path -LiteralPath $resolvedExecutable -PathType Leaf)) {
    throw 'ExecutablePath must identify a Windows executable file.'
}
if ([System.IO.Path]::GetExtension($resolvedExecutable) -ine '.exe') {
    throw 'ExecutablePath must have the .exe extension.'
}

if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repositoryRoot 'dist'
}
$resolvedOutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
[System.IO.Directory]::CreateDirectory($resolvedOutputDirectory) | Out-Null

if ([string]::IsNullOrWhiteSpace($InnoCompilerPath)) {
    # PATH解決のみを行い、インストール先を推測しない。CIや開発機で
    # 使うコンパイラを明示的に切り替えられるようにする。
    $resolvedInnoCompiler = (Get-Command ISCC.exe -CommandType Application -ErrorAction Stop).Source
}
else {
    $resolvedInnoCompiler = (Resolve-Path -LiteralPath $InnoCompilerPath -ErrorAction Stop).Path
}
if (-not (Test-Path -LiteralPath $resolvedInnoCompiler -PathType Leaf)) {
    throw 'InnoCompilerPath must identify ISCC.exe.'
}

$metadataJson = & cargo metadata --locked --filter-platform $TargetTriple --format-version 1 `
    --manifest-path (Join-Path $repositoryRoot 'Cargo.toml')
if ($LASTEXITCODE -ne 0) {
    throw "cargo metadata failed with exit code $LASTEXITCODE."
}
$metadata = $metadataJson | ConvertFrom-Json
$workspacePackageIds = @($metadata.workspace_members)
$workspacePackage = $metadata.packages |
    Where-Object { $workspacePackageIds -contains $_.id } |
    Select-Object -First 1
if ($null -eq $workspacePackage) {
    throw 'Could not identify the LunaPDF workspace package from Cargo metadata.'
}
$version = [string] $workspacePackage.version
$installer = Join-Path $resolvedOutputDirectory "LunaPDF-Setup-$version-x64.exe"
$portableFolderName = "LunaPDF-Portable-$version-x64"
$portableZip = Join-Path $resolvedOutputDirectory "$portableFolderName.zip"
$existingOutputs = @(@($installer, $portableZip) |
    Where-Object { Test-Path -LiteralPath $_ })
# 既存2成果物を上書きすると、2件目の公開失敗時に世代が混在する。公開先は
# 空であることを要求し、この実行が作ったファイルだけを失敗時に除去できるようにする。
if ($existingOutputs.Count -ne 0) {
    throw "Refusing to replace existing distribution output: $($existingOutputs -join ', ')"
}

$commit = (& git -C $repositoryRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($commit)) {
    throw 'Could not identify the source commit for the distribution.'
}
$workingTreeStatus = @(& git -C $repositoryRoot status --porcelain --untracked-files=normal)
if ($LASTEXITCODE -ne 0) {
    throw 'Could not inspect the source working tree for the distribution.'
}
# SOURCE-CODE.txtが指すcommitと実際のビルド入力を一致させるため、
# 未コミットのソースを含む配布物は作成しない。
if ($workingTreeStatus.Count -ne 0) {
    throw 'The source working tree must be clean before building a distribution.'
}
$executableVersionInfo = (Get-Item -LiteralPath $resolvedExecutable).VersionInfo
$expectedProvenance = "SourceCommit=$commit;Dirty=false"
# build.rsがコンパイル時に埋め込んだcommitを照合し、別commitの古いEXEへ
# 現在のHEADを対応ソースとして誤表示しない。
if ($executableVersionInfo.Comments -cne $expectedProvenance) {
    $actualProvenance = $executableVersionInfo.Comments
    throw "Executable source provenance mismatch. Expected '$expectedProvenance', found '$actualProvenance'."
}
if ($executableVersionInfo.ProductName -cne 'LunaPDF' -or
    $executableVersionInfo.ProductVersion -cne $version) {
    throw "Executable product metadata does not match LunaPDF $version."
}
$executableHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $resolvedExecutable).Hash

$licenseAssetRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot 'third-party-license-assets')).Path
$licenseManifestPath = Join-Path $licenseAssetRoot 'manifest.json'
$licenseAssetManifest = @(Get-Content -Raw -LiteralPath $licenseManifestPath | ConvertFrom-Json)
$licenseAssetsByPackage = @{}
$licenseAssetRootPrefix = $licenseAssetRoot.TrimEnd('\') + '\'
foreach ($entry in $licenseAssetManifest) {
    $packageKey = "{0}@{1}" -f $entry.package, $entry.version
    if ($licenseAssetsByPackage.ContainsKey($packageKey)) {
        throw "Duplicate third-party license asset mapping: $packageKey"
    }
    $mappedFiles = [System.Collections.Generic.List[System.IO.FileInfo]]::new()
    foreach ($relativePath in @($entry.files)) {
        $assetPath = [System.IO.Path]::GetFullPath((Join-Path $licenseAssetRoot ([string] $relativePath)))
        # manifestの誤記で配布元リポジトリの任意ファイルを取り込まないよう、
        # 固定ライセンス資産ディレクトリ内の通常ファイルだけを許可する。
        if (-not $assetPath.StartsWith($licenseAssetRootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "Third-party license asset leaves its allowed root: $relativePath"
        }
        if (-not (Test-Path -LiteralPath $assetPath -PathType Leaf)) {
            throw "Third-party license asset is missing: $assetPath"
        }
        $mappedFiles.Add((Get-Item -LiteralPath $assetPath))
    }
    $licenseAssetsByPackage[$packageKey] = $mappedFiles
}

$stagingRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("lunapdf-distribution-" + [guid]::NewGuid().ToString('N'))
$portableRoot = Join-Path $stagingRoot $portableFolderName
$licenseRoot = Join-Path $portableRoot 'licenses'
[System.IO.Directory]::CreateDirectory($licenseRoot) | Out-Null

try {
    Copy-Item -LiteralPath $resolvedExecutable -Destination (Join-Path $portableRoot 'LunaPDF.exe')
    $projectLicensePath = Join-Path $repositoryRoot 'LICENSE.txt'
    Copy-Item -LiteralPath $projectLicensePath -Destination (Join-Path $portableRoot 'LICENSE.txt')
    $distributionReadmePath = Join-Path $PSScriptRoot 'distribution-readme.txt'
    Copy-Item -LiteralPath $distributionReadmePath -Destination (Join-Path $portableRoot 'README.txt')

    $sourceArchiveName = "LunaPDF-source-$commit.zip"
    $sourceArchivePath = Join-Path $portableRoot $sourceArchiveName
    & git -C $repositoryRoot archive --format=zip "--output=$sourceArchivePath" HEAD
    if ($LASTEXITCODE -ne 0) {
        throw "git archive failed with exit code $LASTEXITCODE."
    }
    $sourceLines = @(
        'LunaPDF corresponding source code',
        '=================================',
        '',
        "Version: $version",
        "Commit: $commit",
        "Build target: $TargetTriple",
        "Executable SHA-256: $executableHash",
        "Bundled source: $sourceArchiveName",
        "Repository: https://github.com/takenoko9973/LunaPDF",
        "Exact source: https://github.com/takenoko9973/LunaPDF/tree/$commit",
        '',
        'The bundled archive contains the committed LunaPDF build scripts and source for this binary.'
    )
    [System.IO.File]::WriteAllLines(
        (Join-Path $portableRoot 'SOURCE-CODE.txt'),
        $sourceLines,
        [System.Text.UTF8Encoding]::new($false)
    )

    $noticeLines = [System.Collections.Generic.List[string]]::new()
    $noticeLines.Add('Third-party packages and license files')
    $noticeLines.Add('======================================')
    $noticeLines.Add('')
    $thirdPartyPackages = $metadata.packages |
        Where-Object { $workspacePackageIds -notcontains $_.id } |
        Sort-Object name, version
    foreach ($package in $thirdPartyPackages) {
        $packageFolderName = ("{0}-{1}" -f $package.name, $package.version) -replace '[^A-Za-z0-9._+-]', '_'
        $packageLicenseRoot = Join-Path $licenseRoot $packageFolderName
        $packageRoot = Split-Path -Parent ([string] $package.manifest_path)
        # ベンダーコードを含むcrate（MuPDFなど）の通知も収集するため、
        # crate直下だけでなく配布パッケージ全体を対象にする。LICENSE-MITのような
        # 区切り文字付き名称と、SPDX慣行のLICENSESディレクトリも対象に含める。
        $licenseFiles = @(Get-ChildItem -LiteralPath $packageRoot -File -Recurse |
            Where-Object {
                $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE|COPYRIGHT)([-_.].*|$)' -or
                $_.FullName -match '[\\/](LICENSES|LICENCES)[\\/]'
            } |
            Sort-Object FullName)
        if (-not [string]::IsNullOrWhiteSpace([string] $package.license_file)) {
            $declaredLicensePath = [string] $package.license_file
            # Cargo metadataのlicense_fileは通常絶対パスだが、相対パスの場合は
            # package root基準というCargoの契約に従って解決する。
            if (-not [System.IO.Path]::IsPathRooted($declaredLicensePath)) {
                $declaredLicensePath = Join-Path $packageRoot $declaredLicensePath
            }
            if (-not (Test-Path -LiteralPath $declaredLicensePath -PathType Leaf)) {
                throw "Cargo metadata declares a missing license file for $($package.name): $declaredLicensePath"
            }
            $licenseFiles += Get-Item -LiteralPath $declaredLicensePath
        }
        $packageKey = "{0}@{1}" -f $package.name, $package.version
        if ($licenseAssetsByPackage.ContainsKey($packageKey)) {
            $licenseFiles += @($licenseAssetsByPackage[$packageKey])
        }
        $licenseFiles = @($licenseFiles | Sort-Object FullName -Unique)
        $licenseExpression = if ([string]::IsNullOrWhiteSpace([string] $package.license)) {
            'not declared in Cargo metadata'
        }
        else {
            [string] $package.license
        }
        $noticeLines.Add(("{0} {1} -- {2}" -f $package.name, $package.version, $licenseExpression))
        if (@($package.authors).Count -ne 0) {
            $noticeLines.Add(("  Authors: {0}" -f (@($package.authors) -join ', ')))
        }
        if (-not [string]::IsNullOrWhiteSpace([string] $package.repository)) {
            $noticeLines.Add(("  Source: {0}" -f $package.repository))
        }
        if ($licenseFiles.Count -eq 0) {
            throw "No reviewed license text was found for Windows dependency $packageKey."
        }
        else {
            [System.IO.Directory]::CreateDirectory($packageLicenseRoot) | Out-Null
            for ($index = 0; $index -lt $licenseFiles.Count; $index++) {
                $licenseFile = $licenseFiles[$index]
                $destinationName = '{0:D3}-{1}' -f ($index + 1), $licenseFile.Name
                $destinationPath = Join-Path $packageLicenseRoot $destinationName
                Copy-Item -LiteralPath $licenseFile.FullName -Destination $destinationPath
                $noticeLines.Add(("  licenses\{0}\{1}" -f $packageFolderName, $destinationName))
            }
        }
        $noticeLines.Add('')
    }
    [System.IO.File]::WriteAllLines(
        (Join-Path $portableRoot 'THIRD-PARTY-LICENSES.txt'),
        $noticeLines,
        [System.Text.UTF8Encoding]::new($false)
    )

    # ZIP形式は1980年より前の時刻を表現できないため、依存crateからコピーした
    # 古いタイムスタンプだけを下限へ丸め、Compress-Archiveの警告を避ける。
    $zipTimestampFloor = [datetime] '1980-01-01T00:00:00'
    Get-ChildItem -LiteralPath $portableRoot -File -Recurse |
        Where-Object { $_.LastWriteTime -lt $zipTimestampFloor } |
        ForEach-Object { $_.LastWriteTime = $zipTimestampFloor }

    $temporaryZip = Join-Path $stagingRoot "$portableFolderName.zip"
    Compress-Archive -LiteralPath $portableRoot -DestinationPath $temporaryZip -CompressionLevel Optimal

    $environmentNames = @('LUNAPDF_VERSION', 'LUNAPDF_PAYLOAD_DIR', 'LUNAPDF_OUTPUT_DIR')
    $previousEnvironment = @{}
    foreach ($name in $environmentNames) {
        $previousEnvironment[$name] = [System.Environment]::GetEnvironmentVariable($name, 'Process')
    }
    try {
        [System.Environment]::SetEnvironmentVariable('LUNAPDF_VERSION', $version, 'Process')
        [System.Environment]::SetEnvironmentVariable('LUNAPDF_PAYLOAD_DIR', $portableRoot, 'Process')
        [System.Environment]::SetEnvironmentVariable('LUNAPDF_OUTPUT_DIR', $stagingRoot, 'Process')
        & $resolvedInnoCompiler /Q (Join-Path $PSScriptRoot 'LunaPDF.iss')
        if ($LASTEXITCODE -ne 0) {
            throw "ISCC failed with exit code $LASTEXITCODE."
        }
    }
    finally {
        foreach ($name in $environmentNames) {
            [System.Environment]::SetEnvironmentVariable($name, $previousEnvironment[$name], 'Process')
        }
    }

    $stagedInstaller = Join-Path $stagingRoot "LunaPDF-Setup-$version-x64.exe"
    if (-not (Test-Path -LiteralPath $stagedInstaller -PathType Leaf)) {
        throw "ISCC completed without creating the expected installer: $stagedInstaller"
    }
    $publishedOutputs = [System.Collections.Generic.List[string]]::new()
    try {
        Move-Item -LiteralPath $stagedInstaller -Destination $installer
        $publishedOutputs.Add($installer)
        Move-Item -LiteralPath $temporaryZip -Destination $portableZip
        $publishedOutputs.Add($portableZip)
    }
    catch {
        # 公開先は事前に空と確認済みなので、ここで消すのはこの実行が作った
        # 成果物だけであり、以前の配布物を失うrollbackにはならない。
        foreach ($publishedOutput in $publishedOutputs) {
            if (Test-Path -LiteralPath $publishedOutput) {
                Remove-Item -LiteralPath $publishedOutput -Force
            }
        }
        throw
    }
    Write-Output "Installer: $installer"
    Write-Output "Portable ZIP: $portableZip"
}
finally {
    # 再帰削除は、この実行がOSの一時フォルダ直下に作った一意な領域に限る。
    $temporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    $resolvedStagingRoot = [System.IO.Path]::GetFullPath($stagingRoot)
    if (-not $resolvedStagingRoot.StartsWith($temporaryRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove a staging directory outside the system temporary directory: $resolvedStagingRoot"
    }
    if (Test-Path -LiteralPath $resolvedStagingRoot) {
        Remove-Item -LiteralPath $resolvedStagingRoot -Recurse -Force
    }
}
