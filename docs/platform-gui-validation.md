# プラットフォームおよびGUI受入記録

## 目的と範囲

設計書のF-01、F-03〜F-09、N-01およびフェーズ1〜5の終了条件について、LunaPDF本体を実行した証拠を残す。検証日は2026-07-19、対象commitは`94223ad`である。

GUI判定では対象ウィンドウの画素をメモリ上でハッシュまたは格子集計した。全画面、カメラ、OSのスクリーンショット機能は使用せず、画像ファイルも保存していない。

## Windowsホスト

| 項目 | 値 |
| --- | --- |
| OS build | Microsoft Windows 10.0.19045.7548 |
| Rust | rustc 1.96.0 |
| target | `x86_64-pc-windows-msvc` |
| build | `cargo build --target x86_64-pc-windows-msvc`、成功 |
| debug exe | 48,909,312 bytes |
| exe SHA-256 | `EBD1C9102EF2DBBEBEA6DC4249AAD67419623A169C303567BEC94C91BD2DDA23` |

固定100ページfixtureをコマンドライン引数にしてMSVC版を起動した。ネイティブウィンドウ`LunaPDF`が作成され、プロセスは応答中、2秒後のRSSは146.7 MiB、通常のウィンドウ終了経路はexit code 0だった。

PDF表示領域を含むウィンドウ内の1,200格子点をWin32の`GetWindowDC`と`GetPixel`で読み、11色、暗色10点、明色1,186点を得た。ウィンドウ寸法は1,216 x 939 pixelsで、単色の空画面ではなくPDFの白いページと文字を描画していることを確認した。ビットマップの作成や保存は行っていない。

`packaging/windows/register-pdf-association.ps1`はビルド済みexeを指定した`-WhatIf`が成功し、レジストリを開く前に登録内容を表示した。通常設定を変えないため、今回の検証ではHKCUへの実登録と既定PDFアプリの変更は行っていない。

現在のホストはWindows 11ではない。MSVCでのビルド、Windows本体GUI、PDF初期表示は確認済みだが、N-01のうちWindows 11実機での最終実行確認は未判定である。

## Linux / WSLg

Debian GNU/Linux 13.6のDev Containerから、WSLgのX11経路でrelease版LunaPDFを起動した。固定fixtureと詳細な性能環境は[性能測定記録](performance-measurement.md)に記載している。

### PDFのドラッグ＆ドロップ

空のLunaPDFウィンドウへ、X11のXDND v5で固定fixtureの`text/uri-list`を送った。アプリはドロップを受理し、URI選択データを取得し、成功した完了応答を返した。

```text
target_version=5 accepted=1 selection_served=1 finished=1 succeeded=1
empty_hash=14816275755230384744
pdf_hash=3752196419373954187
```

空画面とドロップ完了後のウィンドウ内画素ハッシュも異なるため、プロトコル応答だけでなくタブ追加とPDF表示まで進んだと判定した。これによりLinux実行環境でF-01のDnD経路を確認した。

### 表示モードとサイドバー

同じ固定fixtureで、ツールバーをポインター操作し、連続表示から単ページ表示、連続表示への復帰、Outlineサイドバーの表示、Thumbnailsへの切り替えを順に実行した。各操作後のLunaPDFウィンドウ内ハッシュは次のとおりで、すべて直前状態から変化した。

```text
continuous=12049848950263834231
single=11275682624395467671
continuous_again=16553649740215444740
outline=3807084979689963422
thumbnails=13426646931318055236
```

これによりF-04、F-07、F-08の実GUI経路を確認した。F-03、F-05、F-06、F-09については、同じウィンドウ操作経路で測った連続スクロール、ページ移動、倍率変更を含むcache plateau、検索入力の結果を[性能測定記録](performance-measurement.md)に記載している。

### Linux関連付けメタデータ

`desktop-file-validate packaging/linux/lunapdf.desktop`はexit code 0だった。desktop entryは`application/pdf`と複数パス用`%F`を宣言する。通常のデスクトップ設定を変更しないため、今回の検証では`xdg-mime default`を実行していない。

## 判定

- 同じRustコードからWindows MSVC版とLinux版をビルドでき、両環境でLunaPDF本体のウィンドウとPDF初期表示を確認した。
- LinuxではDnD、連続/単ページ、Outline/Thumbnails、ページ移動、検索、スクロール、キャッシュ上限到達まで実GUI経路を確認した。
- Windows/Linuxの関連付けメタデータは構文と非破壊プレビューに合格した。
- N-01のWindows 11実機、および異なるDPIの複数モニターは現在の環境では最終判定できない。実装はDPIをcache keyへ含め、密度変更時に世代を失効させるテストを持つが、実機受入は別途必要である。
