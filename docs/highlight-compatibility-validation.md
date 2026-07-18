# Highlight保存互換性の検証

記録日：2026-07-18
対象commit：`7b06502`

## fixture生成

通常のテストでは一時PDFを削除するため、外部検査用PDFはignoredテストを明示実行して生成する。`LUNAPDF_ACCEPTANCE_OUTPUT`には意図しない作業ツリー内出力を避けるため絶対パスを指定する。既存ファイルを指定した場合は上書きせず失敗する。

```sh
LUNAPDF_ACCEPTANCE_OUTPUT="$PWD/target/acceptance-highlight.pdf" \
  cargo test exports_highlight_fixture_for_external_viewers -- --ignored --nocapture
```

このテストは、実際のテキスト座標から選択Quadを取得し、LunaPDFのHighlight作成、保存、再オープン検証を通したPDFを出力する。生成物はテスト成果物であり、リポジトリへコミットしない。

## qpdfによる独立検査

ホストのqpdf 11.9.1で、生成した1,785 bytesのPDFを検査した。SHA-256は`A8D038192389075277FFCF63B02B5FD116533E3DD2F80D7689A9AF079607C48C`だった。

```text
PDF Version: 1.7
File is not encrypted
File is not linearized
No syntax or stream encoding errors found
```

`qpdf --json`では、1ページ目の注釈に次の独立した構造を確認した。

- `/Type /Annot`
- `/Subtype /Highlight`
- 黄色を表す`/C [1 1 0]`
- 選択範囲を表す`/QuadPoints`
- `/AP`外観ストリーム

これにより、PDF構造検査と標準Highlight注釈の格納は確認済みである。

## Popplerによる独立描画

Dev ContainerのPoppler 25.03.0を使い、注釈を隠さない既定設定で1ページ目をPNGへ描画した。

```sh
pdftoppm -png -f 1 -singlefile -r 150 \
  target/acceptance-highlight.pdf \
  target/acceptance-highlight-poppler
```

描画結果では、文字列`LunaPDF external viewer highlight`と同じ位置に黄色のHighlightが表示された。これにより、MuPDFとは独立したLinux PDF実装で注釈のページ、位置、色、外観を確認済みである。

## SumatraPDFによるWindows表示確認

2026-07-19にWindows 11上のSumatraPDF 3.6.1でfixtureを開いた。一時appdataを指定して通常設定とセッションを変更せず、1ページ目をfit pageで表示した。

ウィンドウ単体の画面キャプチャでは、文字列`LunaPDF external viewer highlight`と同じ位置に黄色のHighlightが表示された。ページ表示は`1 / 1`で、LinuxのPoppler描画と位置、色、ページが一致した。閲覧後のfixture SHA-256は生成時と同じ`A8D038192389075277FFCF63B02B5FD116533E3DD2F80D7689A9AF079607C48C`であり、表示確認ではPDFを変更していない。

画面キャプチャ`target/acceptance-highlight-sumatra-window-only.png`は検証成果物としてリポジトリへコミットしない。

## 未完了の互換性確認

設計書15.3の完全な互換性確認には、次を確認する必要がある。

- 外部ビューアで再保存した後もHighlightが残る
- Linuxの対象デスクトップ環境にあるGUIビューアでも同じ位置と色に表示される

WindowsのSumatraPDF表示とLinuxの独立したPoppler描画は合格している。外部ビューアによる再保存とLinux GUI操作は、対象ビューアを備えた環境で別途確認する。
