# LunaPDF Windows配布対応 作業報告

## 変更概要

- Windows release EXEへGUI subsystem、製品情報、複数解像度アイコン、source commitとdirty状態を埋め込んだ。debug buildのコンソール動作とWindows以外の起動経路は維持した。
- 複数のコマンドライン引数を既存タブ追加経路へ渡し、同一Windowsユーザーの2回目以降の起動は保護したNamed Pipe経由で既存ウィンドウへ転送する方式にした。UI側では既存の`open_document`を再利用し、最小化解除とfocus要求を行う。
- Inno Setup 6で管理者権限不要のper-userインストーラーを作る。`HKCU`だけへOpen With／Default Apps候補を登録し、`UserChoice`は変更しない。
- 同じrelease EXEからインストーラーとポータブルZIPを組み立てる。Cargo metadataのversion、Windows targetの依存集合、EXEへ埋め込んだsource provenanceを配布スクリプトで照合する。
- AGPLの対応ソースarchiveと、対象依存253パッケージのライセンス本文を両配布物へ同梱する。取得不能な本文を無視せず、配布生成を失敗させる方式にした。
- push／pull request用Windows CIと、ブランチを選べる`workflow_dispatch`専用配布workflowを分離した。外部Actionは公式`actions/*`の完全commit SHAへ固定し、権限は`contents: read`、checkout credentialは保持しない。

この構成は、1つのversionと1つのrelease EXEを基準にして配布形態間の差異を減らし、既定アプリの強制変更や推測fallbackを避けるために採用した。

## 変更ファイル

- `Cargo.toml`／`Cargo.lock`: Windows IPC、UTF-16、Windows resource生成の依存と必要な`windows-sys` featureを追加した。
- `build.rs`: Windowsアイコン、製品情報、source commit／dirty状態をEXEへ埋め込み、Gitとビルド入力の変更で再生成する。
- `assets/windows/`: ウィンドウ用PNGとEXE／installer用ICOを保持する。
- `src/main.rs`: 複数起動引数の収集、単一インスタンスの取得／転送、外部open channelを構成する。
- `src/platform/windows/single_instance.rs`: ユーザー単位Named Pipe、DACL、相互SID検証、要求protocol、UI queue投入後の受領確認、受信threadを担当する。
- `src/platform/windows/default_apps.rs`: Windowsの既定アプリ設定画面を開く。
- `src/app.rs`／`src/app/tests.rs`: 外部open要求を通常のタブ追加経路へ接続し、終了競合、focus要求、入力境界を検証する。
- `LICENSE.txt`: LunaPDFのAGPL-3.0-only本文を配布可能な形で保持する。
- `packaging/windows/LunaPDF.iss`: per-user install、shortcut、Open With／Default Apps登録、uninstallを定義する。
- `packaging/windows/build-distributions.ps1`: version／provenance検査、payload、source、依存license、ZIP、installer、2成果物の公開を一括して行う。
- `packaging/windows/register-pdf-association.ps1`: 手動配置EXEの初回Open With登録を、同一ユーザー排他と失敗時rollback付きで行う。
- `packaging/windows/distribution-readme.txt`: 配布物内の利用説明を保持する。
- `packaging/windows/third-party-license-assets/`: crate内に本文がない依存へ、version固定で確認した補足licenseを対応付ける。
- `.github/workflows/ci.yml`: push／pull requestのWindows MSVC format、test、Clippyを実行する。
- `.github/workflows/windows-distribution.yml`: 手動のWindows MSVC release、2配布物生成、Artifact uploadを実行する。
- `docs/windows-distribution.md`: 利用、ローカル／Actions生成、version更新、uninstall、制約の恒常手順を説明する。
- `docs/tasks/distribution/`: 元の指示書と本報告を記録する。

## 依存関係

- `interprocess = "2.4.2"`（lock: 2.4.3）: Windows Named Pipeをlocal socketとして同期listen／connectする。Windows targetだけへ追加した。
- `widestring = "1.2.1"`: Windows security APIのUTF-16／NUL終端文字列を安全に扱う。Windows targetだけへ追加した。
- `winresource = "0.1.31"`: Cargo version、アイコン、製品情報、source provenanceをWindows resourceへ埋め込むbuild dependencyとして追加した。
- `windows-sys`: 新規crateではなく、Named Pipe peerのprocess token／SID検査と受領確認待機に必要なSecurity、Authorization、Pipes、Threading featureを追加した。
- Inno Setup 6: Rust依存にはせず、Windowsのローカル配布生成と`windows-2022` runnerで使う外部compilerとした。
- GitHub Actions: `actions/checkout` v6.0.2と`actions/upload-artifact` v7.0.1を公式tagの完全SHAで参照する。secretとwrite権限は追加していない。

## 配布物

- インストーラー: `LunaPDF-Setup-0.1.0-x64.exe`、11,319,005 bytes、SHA-256 `CC842328CA0132C6A9964A4FB382A5F89CDA5F40597683727FB88EFCEDEEF352`
- ポータブル版: `LunaPDF-Portable-0.1.0-x64.zip`、13,993,564 bytes、SHA-256 `BEC86719E76ED7424A69B3772E94E46CB01B3525B3129731427C4D342207DA31`
- 入力EXE: `lunapdf-release.exe`、24,990,720 bytes、SHA-256 `BBDE3A027BD5E40D6235F03886AA2DAFD7FDB8BCA75B86A94626A25EAB4D11F8`

上記のローカル成果物はcommit `08c3f037eafb7bb1e9439ea39a5c74c692d39559`のclean working treeから作り、`dist`へ保存した。生成方法は[Windows 配布ガイド](../../windows-distribution.md)に記載した。GitHub Actionsでは同じ2ファイルをArtifact `LunaPDF-Windows-<commit SHA>`へ保存する設計である。

## 検証結果

### 実行した主なコマンド

```text
docker compose -f .devcontainer/compose.base.yml exec workspace cargo fmt --check
docker compose -f .devcontainer/compose.base.yml exec workspace cargo check
docker compose -f .devcontainer/compose.base.yml exec workspace cargo test
docker compose -f .devcontainer/compose.base.yml exec workspace cargo clippy --all-targets
docker compose -f .devcontainer/compose.base.yml exec workspace cargo check --target=x86_64-pc-windows-gnu
docker compose -f .devcontainer/compose.base.yml exec workspace sh -c "cargo build --release --target=x86_64-pc-windows-gnu && install -D target/x86_64-pc-windows-gnu/release/lunapdf.exe /workspace/dist/lunapdf-release.exe"
pwsh -NoProfile -File packaging\windows\build-distributions.ps1 -ExecutablePath dist\lunapdf-release.exe -TargetTriple x86_64-pc-windows-gnu -OutputDirectory dist -InnoCompilerPath <ISCC.exe>
cargo check --locked --target x86_64-pc-windows-msvc
```

PowerShell process harnessで、release EXEの起動、複数PDF転送、installerのsilent install／同一版上書き／uninstall、Portableの展開／起動／通常終了も実行した。workflowはPyYAMLで構文を読み、全PowerShell `run` blockをPowerShell AST parserへ渡した。

### 成功した検証

- Linuxのformat、check、test、ClippyとWindows GNU checkが成功した。`cargo test`は254 passed、0 failed、2 ignoredで、既存のタブ、分割、PDF表示、注釈、session系を含むテストに、この変更による失敗はなかった。
- Windows GNU release buildが成功し、PEはWindows GUI subsystem、製品名`LunaPDF`、version `0.1.0`、アイコンを持つ。Commentsは`SourceCommit=08c3f03...;Dirty=false`で、入力EXEのSHA-256は上記と一致した。
- 空白と日本語を含む複数PDFを主プロセスと2回目のプロセスへ渡し、2回目が既存ウィンドウへ転送して終了することを確認した。protocol、異常入力、同一ユーザー認証、UI queue投入後の受領確認、2 clientの同時要求はWindows実機の対象自動テスト10件でも確認した。
- 手動関連付けは`-WhatIf`、mutex競合拒否、実登録、逆順cleanupを確認した。Open With値は`REG_NONE`で、実行前後の既定PDFは`SumatraPDF.pdf`、`UserChoice` hashは`r78V97BfdGQ=`のままだった。
- Inno Setup 6.7.3で2成果物を生成した。Portable内EXEと入力EXEのSHA-256は一致し、対応ソースarchiveは`Cargo.lock`、`build.rs`、Windows assets、配布script、license manifestを含んだ。
- Windows target依存253パッケージすべてにlicense directoryがあり、空のlicense fileは0件だった。
- Windows 10 Pro 22H2 build 19045.7548で、installerの新規導入と同一版上書きはいずれもexit 0、導入EXEは入力EXEと一致した。uninstallもexit 0で、install directory、LunaPDF専用ProgID、Applications、Capabilities、RegisteredApplications、Open With値は残らなかった。
- installer／uninstallerの前後で既定PDFのProgIDとhash、および既存`%APPDATA%\LunaPDF\session.json`のSHA-256は変化しなかった。
- Portableは展開後にウィンドウを表示し、初期化待ち後の通常closeを3回連続でexit 0として確認した。同一EXEを使うためinstaller版との実行機能差はない。
- 既存出力がある状態で配布scriptはexit 1となり、installer／ZIPのhashを変更しなかった。
- workflow 2件はYAML parse成功、PowerShell block 7件はparse error 0だった。Action SHA、`contents: read`、credential非保持、Artifact対象2ファイルは独立レビューでも指摘なしだった。

### 失敗した検証と切り分け

- サンドボックス内のローカルMSVC checkは`link.exe`が`T:\Temp`へ一時ファイルを作れず失敗した。サンドボックス外ではMuPDF native buildまで進み、このホストに`libclang.dll`がないためbindgenで失敗した。GitHub `windows-2022` inventoryにはLLVM 20.1.8があり、workflowは`C:\Program Files\LLVM\bin\libclang.dll`を事前確認するが、workflow自体の実走結果ではない。
- 最初のPortable自動closeは、最初のwindow handleを得た直後にcloseして10秒で終了せず、検証processを停止した。起動初期化後2秒待つ条件へ修正すると3回連続でexit 0だった。製品の通常終了失敗とは判定していないが、起動直後すぎる自動操作は受入条件に使わない。

### 実機で確認した項目

- Windows 10上のrelease GUI起動、空白／日本語パスの複数PDFと単一インスタンス転送
- installerの管理者昇格なし導入、同一版上書き、payload／registry、uninstall、既定PDFとsessionの不変
- Portableの展開、GUI起動、通常終了

### 未確認の項目

- Windows 11実機でのinstall、Open With表示、Default Apps UIからの手動選択、Explorerアイコン表示
- GitHub-hosted `windows-2022`上でのCI／配布workflowの実走とArtifact download
- installer UIでデスクトップshortcutを選んだ実操作、およびスタートメニューshortcutからの起動
- 最小化／背面状態から外部openしたときのOS前面化制限を含む実GUI挙動
- ARM64、32bit、全ユーザー導入、企業配布（いずれも対象外）

## 残課題

- 現在の成果物は未署名で、SmartScreen等が警告する可能性がある。一般公開前にはコード署名証明書、秘密鍵の保管、署名後hash／provenance、timestamp方針が必要である。
- 自動更新とGitHub Release公開はない。将来はtagと`Cargo.toml` versionの一致検査、署名済み2成果物、checksum、release notesを揃えたうえで、明示的なwrite permissionを配布jobだけへ限定する必要がある。
- Windows 11実機およびGitHub-hosted runnerのend-to-end結果を取得し、未確認項目を別の検証記録で閉じる必要がある。
- 完全ポータブル設定、MSIX／MSI／Store、preview／thumbnail handler、shell extension、企業向けsilent配布は今回の範囲外である。
