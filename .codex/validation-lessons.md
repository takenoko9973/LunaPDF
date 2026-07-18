# LunaPDF検証メモ（Codex向け）

## 目的

2026-07-19の受入検証で判明した「製品の不具合ではなく、検証方法の選択や判定そのものに問題があった事例」を残す。今後のCodex作業では、同じ手段を無批判に再利用せず、観測した事実と、その手段では証明できない範囲を分ける。

この文書は製品仕様ではない。検証手順を変更するときの注意事項と、過去結果の有効範囲を示す内部メモである。

## 無効だった、またはそのままでは判定に使えなかった検証

### `xdotool windowclose`を通常の終了操作として使った

- `xdotool windowclose`は通常のclose requestではなく、X11ウィンドウを直接`XDestroyWindow`する。
- この直後にwinitが`BadWindow`でpanicしたが、通常の終了確認ダイアログや`CancelClose`経路の不具合を示す結果ではない。
- この試行のpanicはLunaPDFのfinding、終了確認のfail、正常終了の証拠のいずれにも使わない。
- 終了確認は`WM_DELETE_WINDOW`を送る小さなXlibクライアント、実際のウィンドウ装飾、または対象WMで動作確認済みの通常close requestを使う。
- WSLgでは`xdotool windowquit`とsynthetic `Alt+F4`が無視された試行もあった。イベントを送っただけで成功とせず、ダイアログ表示、ウィンドウ存続、プロセス終了を別々に観測する。

### システムクリップボードの既存値を選択結果の判定に使おうとした

- 一度、既存クリップボードを読んだが値は変化せず、選択や`Ctrl+C`の成否を判定できなかった。
- 共有状態であるクリップボードの既存値は、今回のアプリ操作に由来する証拠ではない。
- ユーザーの明示許可なしに、今後の自動検証でシステムクリップボードを読み書きしない。
- F-10のGUI受入で確認済みなのは、ドラッグ後にstatusへ論理選択文字列`QPDF`が表示されたことまでである。クリップボード転送を別途検証していないことを隠さない。

### OSのスクリーンショット機能を使った

- OSのキャプチャ通知音が鳴り、全画面や他ウィンドウを取得していないかユーザーを不安にさせた。
- 今後のGUI検証では、OSのスクリーンショット機能を使わない。全画面、カメラ、マイクも使わない。
- Windowsでは対象LunaPDFの`HWND`だけを`GetWindowDC` / `GetPixel`で読む。X11では対象window idの画素だけをメモリ上でhash、格子集計、必要最小限のOCRに使う。
- 画像ファイルを保存しない。画像が本当に必要な別タスクでは、対象ウィンドウだけに限定することを事前に明示する。

### synthetic dragを速く送りすぎた

- 単一または短すぎるpointer移動では、egui側が意図したドラッグ範囲として扱わない場合があった。
- 選択検証では、押下後に複数の中間move eventを時間を空けて送り、release後のstatus文字列またはselection stateで成立を確認する。
- pointer eventを送信できたこと自体を、選択成功の証拠にしない。

### 強制終了を通常終了の証拠に混ぜた

- WSLgが通常close requestを無視した性能測定では、測定完了後に、その試行で起動した一時プロセスだけを明示的に終了した。
- この強制終了は後片付けであり、正常終了、保存、終了確認ダイアログの証拠ではない。
- 後片付けの強制終了と、受入対象の終了操作を結果記録で分離する。

## 過大に解釈してはいけない測定

### WindowsのOS要件

- 当初Windows 11として扱いかけたが、実ホストは`Microsoft Windows 10.0.19045.7548`だった。
- OS名やversionは推測せず、実行時にホストから取得する。
- 現在の証拠はWindows 10上のMSVC buildとGUI起動であり、Windows 11実機の受入を証明しない。

### cold起動

- page cacheを含む対象PDFのcold試行は行ったが、共有ライブラリ、フォント、X11、GPU driverまでcoldになった完全OS coldではない。
- `drop_caches`できない環境の結果をN-03/N-04の完全cold達成と書かない。`warm`、`対象ファイルcold`、`完全OS cold未測定`を分ける。

### メモリとGPU LRU

- プロセスRSSとアプリ内LRUのbyte表示は測定したが、GPU driver側の実メモリは測っていない。
- statusの`render scale`は、同一ページに残る別generationのtileを指して古く見える場合があった。LRU到達判定にはtile count、cache bytes、新しいcache keyの継続投入、RSS plateauを使い、render scaleだけに依存しない。
- `49d8aa1`より前のGPU stress 570.410 MiB、thumbnail 314.910 MiBは、RGBA転送`Vec`のcapacity保持を含む修正前の値である。現在値として再利用しない。
- 実装修正後は必ず再測定し、測定対象commitを記録する。現在の再測定値はGPU stress 403.586 MiB、thumbnail 180.277 MiBである。

### OS関連付け

- Windowsの`-WhatIf`とLinuxの`desktop-file-validate`は、登録内容や構文の非破壊確認である。
- HKCUへの実登録、既定PDFアプリの変更、`xdg-mime default`は行っていないため、実際の既定アプリ切替まで確認済みとは書かない。

## 今回有効と判断したGUI検証手段

- 対象ウィンドウだけのin-memory hash、格子画素集計、必要最小限のOCR。
- X11 XDND v5のaccepted、selection transfer、finished、success応答と、drop前後の対象ウィンドウhash差分の組合せ。
- `WM_DELETE_WINDOW`による通常終了要求と、Cancel / Discard / Saveごとのウィンドウ存続・プロセス終了・PDF hashの組合せ。
- 元fixtureではなく一時コピーを使った保存と、保存前後SHA-256、qpdf構文検査、Highlight annotation件数の組合せ。
- 操作前後の単一の画面差だけでなく、status、ファイル、プロセス、protocol応答のうち、その要件に直接対応する複数の観測。

## Codexが結果を採用する前のチェック

1. 送ったイベントは、ユーザーが行う通常操作と同じ意味か。強制破棄や強制終了ではないか。
2. 観測値は今回の操作で変化したものか。既存クリップボードなどの共有状態ではないか。
3. ツールがイベントを送った事実ではなく、アプリ側の状態変化を確認したか。
4. 元PDF、レジストリ、既定アプリ、他プロセスなど、検証対象外の状態を変更していないか。
5. 対象window id、対象commit、OS、build target、fixture hashを記録したか。
6. 測れていない範囲をpassに含めていないか。
7. コード変更後に、変更前の性能値やGUI結果をそのまま流用していないか。
8. 無効な試行は結果から除外し、必要なら「方法が無効だった」と明記したか。

## 現在も環境上未判定の項目

- Windows 11実機。
- 異なるDPIの複数モニター間移動。
- 完全OS coldの起動時間と初回ページ表示時間。
- GPU driver側の実メモリ。
- GUI操作による`Ctrl+C`からシステムクリップボードまでの転送。

これらは実装失敗を意味しないが、現在の証拠でpassとも書かない。
