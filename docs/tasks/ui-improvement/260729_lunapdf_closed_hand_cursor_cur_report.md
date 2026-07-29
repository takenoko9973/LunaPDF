# LunaPDF 独自カーソル撤去・標準カーソル化 作業報告

## 1．経緯

当初は閉じた手の独自CURを追加する指示だったが、実装後のユーザー指示により
独自カーソルを廃止し、最終仕様を次のとおり変更した。

- 中ボタンによる自動スクロールでは、カーソルを専用の十字表示へ変更しない。
- 独自の閉じた手へ変更していた成立済みパン中は、標準の上下左右矢印を使う。

## 2．最終実装

`src/ui/cursor.rs::set_pdf_cursor`は、eguiの論理カーソルを設定するだけの処理へ
戻した。Windows限定の`set_cursor_image`、CUR解析、RGBAキャッシュ、
DPI別フレーム選択は残していない。

`src/ui/viewport.rs::pdf_cursor_icon`の最終状態は次のとおり。

| 状態 | カーソル |
|---|---|
| 通常閲覧、空白、背景、注釈、画像、フォーム | `Default` |
| PDF本文の文字上 | `Text` |
| リンク上 | `PointingHand` |
| 左ドラッグがパン閾値未満 | 直下の通常カーソル |
| 左ドラッグがパン閾値を超えた後 | `AllScroll` |
| 中ボタン自動スクロール中 | 直下の通常カーソルを維持 |

成立済みパン中は文字上を通過しても`AllScroll`を維持する。
自動スクロール状態はカーソル選択を上書きしない。

## 3．撤去したもの

- `assets/cursors/grabbing.svg`
- `assets/cursors/grabbing.cur`
- `assets/cursors/README.md`
- `examples/generate_grabbing_cursor.rs`
- `Cargo.toml`の直接`resvg`開発依存
- `Cargo.lock`のLunaPDFパッケージからの直接`resvg`参照
- CUR構造解析、DIB変換、DPI選択、独自カーソル用テスト

`resvg`自体は`egui_extras`のSVG機能が使用する既存の推移的依存として
lockfileに残る。

## 4．維持した動作

- 空白および灰色背景からのパン成立条件。
- パン閾値未満ではカーソルを変えない動作。
- パン終了後に直下の対象へ戻る動作。
- 文字、リンク、注釈の判定。
- 自動スクロールの開始・停止およびスクロール処理。
- 注釈エディタなど、PDF領域外の専用カーソル所有権。
- 非Windowsを含むegui標準カーソル経路。

カーソル表示のための新しい入力状態、fallback、互換処理は追加していない。

## 5．検証

```text
docker compose -f .devcontainer/compose.base.yml exec workspace cargo fmt --check
# 成功

docker compose -f .devcontainer/compose.base.yml exec workspace cargo test cursor_
# 成功：2 passed

docker compose -f .devcontainer/compose.base.yml exec workspace cargo test
# 成功：219 passed, 0 failed, 2 ignored

docker compose -f .devcontainer/compose.base.yml exec workspace cargo check --release
# 成功

docker compose -f .devcontainer/compose.base.yml exec workspace cargo clippy --all-targets
# 成功

docker compose -f .devcontainer/compose.base.yml exec workspace cargo build --target=x86_64-pc-windows-gnu
# 成功
```

追加・更新したカーソルテストでは次を確認した。

- 空白と背景の通常時は`Default`。
- パン成立中は`AllScroll`。
- パン成立中に文字上を通過しても`AllScroll`。
- 自動スクロール中の空白・背景は`Default`のまま。
- 自動スクロール中の文字上は`Text`のまま。
- 自動スクロール中のリンク上は`PointingHand`のまま。

Windows GUI上での目視操作は今回実施していない。
