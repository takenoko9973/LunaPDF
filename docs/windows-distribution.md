# Windows 配布ガイド

## 対象

LunaPDF の Windows 配布物は x86_64 向けの次の2種類である。

| 形式 | ファイル名 | 導入先・起動方法 | PDF関連付け | 設定・セッション |
| --- | --- | --- | --- | --- |
| インストーラー | `LunaPDF-Setup-<version>-x64.exe` | `%LOCALAPPDATA%\Programs\LunaPDF` | Open With候補と既定アプリ候補へユーザー単位で登録 | `%APPDATA%\LunaPDF\session.json` |
| ポータブル | `LunaPDF-Portable-<version>-x64.zip` | 任意の場所へ展開して `LunaPDF.exe` を起動 | 登録しない | `%APPDATA%\LunaPDF\session.json` |

ポータブル版はインストール不要版であり、設定までZIP内に閉じる完全ポータブルモードではない。ZIPを削除してもセッションはユーザー領域に残る。両形式には同じrelease EXEを収録する。

## ローカルで配布物を作る

### 前提

- Git working treeがcleanであること
- Dev Containerが起動済みであること
- Windows側でInno Setup 6の`ISCC.exe`を実行できること
- 生成先に同じバージョンのインストーラーまたはZIPが存在しないこと

配布スクリプトは、現在のcommit、EXEへ埋め込んだcommit、working treeのclean状態を照合する。古いEXEへ別commitのソースを対応付けることはできない。

### 1. Windows GNU release EXEを作る

リポジトリルートで次を実行する。

```powershell
docker compose -f .devcontainer/compose.base.yml exec workspace sh -c "cargo build --release --target=x86_64-pc-windows-gnu && install -D target/x86_64-pc-windows-gnu/release/lunapdf.exe /workspace/dist/lunapdf-release.exe"
```

出力は`dist/lunapdf-release.exe`である。Windows releaseではGUI subsystemを使うため、通常起動時にコンソールを作らない。debug buildはコンソールを維持する。

### 2. インストーラーとZIPを作る

Inno Setupの`ISCC.exe`が`PATH`にある場合は次を実行する。

```powershell
.\packaging\windows\build-distributions.ps1 `
  -ExecutablePath .\dist\lunapdf-release.exe `
  -TargetTriple x86_64-pc-windows-gnu `
  -OutputDirectory .\dist
```

`PATH`にない場合は`-InnoCompilerPath C:\Path\To\ISCC.exe`を追加する。出力先に同名成果物がある場合、スクリプトは上書きせず終了する。別の空ディレクトリを指定するか、既存成果物を退避してから再実行する。

成功すると`dist`に次を作る。

- `LunaPDF-Setup-<version>-x64.exe`
- `LunaPDF-Portable-<version>-x64.zip`

両配布物にはLunaPDFのライセンス、対応ソースcommitのアーカイブ、依存パッケージのライセンス本文を含める。配布スクリプトは対象Windows tripleの全依存について本文が得られない場合に失敗する。

## GitHub Actionsで配布物を作る

1. GitHubのリポジトリで **Actions** を開く。
2. **Windows distribution** を選ぶ。
3. **Run workflow** から対象ブランチを選んで実行する。
4. 完了後、workflow runのArtifact `LunaPDF-Windows-<commit SHA>`を取得する。

Artifactに入るのはバージョン付きインストーラーとポータブルZIPの2ファイルだけである。このworkflowは手動実行専用で、GitHub Releaseの作成や公開は行わない。通常のpush／pull requestでは別の **CI** workflowがWindowsのformat、test、Clippyを実行する。

## バージョンを更新する

バージョンの情報源は`Cargo.toml`の`[package].version`である。

1. `Cargo.toml`の`version`を更新する。
2. Dev Containerで`cargo check`を実行し、必要な`Cargo.lock`更新を含める。
3. 変更をコミットし、working treeをcleanにする。
4. release EXEと配布物を作り直す。

Windowsリソース、インストーラーの表示バージョン、配布物ファイル名はCargo metadataから導出される。これらへ同じ値を手作業で重複記入しない。

## インストールと既定アプリ

インストーラーは管理者権限を要求せず、現在のユーザーだけへ導入する。スタートメニューのショートカットを作り、デスクトップショートカットはインストール画面で選択した場合だけ作る。

インストールだけでは既定のPDFアプリを変更しない。既定にする場合は、LunaPDFの **ファイル → 既定の PDF アプリを設定…** からWindows設定を開き、`.pdf`にLunaPDFを選ぶ。インストーラーとアプリは保護対象の`UserChoice`を直接変更しない。

手動配置した`LunaPDF.exe`をOpen With候補へ登録する場合だけ、次を利用できる。

```powershell
.\packaging\windows\register-pdf-association.ps1 -ExecutablePath C:\Path\To\LunaPDF.exe
```

このヘルパーは初回登録専用で、既存のLunaPDF登録を上書きしない。`-WhatIf`で書込み前に確認できる。

## アンインストール後に残るもの

アンインストーラーは、インストールしたファイル、ショートカット、LunaPDF専用のOpen With／Default Apps登録を削除する。他アプリのPDF登録と既定アプリ選択は変更しない。

現在、永続化するユーザーデータは`%APPDATA%\LunaPDF\session.json`である。開いていたタブ、表示状態、注釈色履歴などを再利用できるよう、通常のアンインストールでは削除しない。完全に消す場合は、LunaPDFが終了していることを確認してからユーザー自身が`%APPDATA%\LunaPDF`を削除する。

## 既知の制約

- コード署名がないため、Windowsが発行元不明の警告を表示する場合がある。
- 自動更新、差分更新、GitHub Release自動公開はない。
- Windows x64のみを対象とし、ARM64、32bit、全ユーザー導入、MSI、MSIX、Microsoft Storeは対象外である。
- PDF preview／thumbnail handlerやExplorer shell extensionは含まない。
- ポータブル版もユーザー領域のセッションを使い、ZIP内だけでは完結しない。
- GitHub-hosted runnerでのworkflow実走とWindows 11実機受入は、2026-08-02時点では未確認である。
