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

## 未完了の互換性確認

設計書15.3の完全な互換性確認には、WindowsとLinuxのGUIビューアで次を確認する必要がある。

- Windowsの標準PDFビューアでも同じ位置と色に表示される
- 外部ビューアで再保存した後もHighlightが残る

ブラウザ自動操作環境はローカル`file:` URLを許可しなかったため、ブラウザ表示を迂回して自動合格扱いにはしていない。WindowsにはSumatraPDFとChromeが存在するため、生成物`target/acceptance-highlight.pdf`を手動で開けばWindows側の残りを確認できる。Linux側の独立描画は合格しているが、GUI操作と再保存は別ビューアを備えた対象デスクトップ環境で確認する。
