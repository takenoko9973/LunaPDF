# PDF キーボードナビゲーション仕様

この文書は矢印キー操作の正式仕様である。**矢印キー操作は実装確認**であり、下記の合成イベントによるヘッドレステストを実行している。GUI／OS の実機入力は未検証である。

## 操作軸とキー割り当て

連続表示と単ページ表示で同じ規則を使う。現在のPDFページの表示幅と実際のviewport幅を比較して横方向を判定し、表示高さとviewport高さを比較して縦方向を判定する。横と縦は独立しており、倍率の値だけでは判定しない。

| キー | 操作軸 | ページ遷移方向 | 遷移先の表示開始位置 | 実装状況 |
| --- | --- | --- | --- | --- |
| ← | 横 | 前ページ | 右端 | 実装確認 |
| → | 横 | 次ページ | 左端 | 実装確認 |
| ↑ | 縦 | 前ページ | 下端 | 実装確認 |
| ↓ | 縦 | 次ページ | 上端 | 実装確認 |
| PageUp | 既存のページ操作 | 前ページ | 既存仕様を維持 | 実装確認 |
| PageDown | 既存のページ操作 | 次ページ | 既存仕様を維持 | 実装確認 |

## 押下と長押しの規則

「長押し」は、新しい押下から解放までを指す。修飾キーなしの押下と、その押下が所有するOSキーリピートだけをPDF操作に使い、1イベントにつき1回だけ処理する。

| 押し始めの状態 | 最初の押下 | 同じ長押しのリピート |
| --- | --- | --- |
| 操作軸でページが表示範囲に収まる | 前後1ページへ移動 | 各イベントで前後1ページへ移動可能 |
| 操作軸でページが表示範囲より大きく、進行方向の端ではない | ページ内をスクロール | ページ内をスクロールし、端で停止。ページを跨ぐには離して押し直す |
| 操作軸でページが表示範囲より大きく、進行方向の端である | 前後1ページへ移動 | 移動先のページ内だけをスクロールし、端で停止。それ以上ページを跨がない |

リピートごとに現在ページと実際のviewportを比較する。収まるページから大きいページへ着いた後は、同じ長押しでページ内をスクロールして端で停止する。大きいページを扱った長押しでは、移動先が小さいページでも再度ページを跨がない。先頭より前、末尾より後へは進まない。1回あたりのスクロール距離やOSキーリピートの時間は、この仕様では固定しない。

遷移時は操作軸の端から表示を始め、直交軸のページ内位置を可能な限り維持する。ページ端には既存のページ余白を含め、文書全体のスクロール上限に収める。

連続表示で矢印が選択したページは、通常の再描画や同じキー操作で表示領域先頭のページに上書きしない。小ページが複数見える場合や、文書先頭・末尾で位置が制限される場合も、選択ページとそのページに対するアンカーを保持する。ホイール、パン、スクロールバーなどが表示位置を変更したときや、PageUp／PageDown、ツールバー、検索などの別の移動操作が位置を所有したときは、既存のページ決定経路へ戻る。この保持状態は一時的な表示状態であり、保存形式を追加しない。

## フォーカスと入力の優先順位

- 検索欄、ページ番号欄、注釈コメント欄などでテキストを編集中は、文字編集側のキー操作を優先する。
- IMEの変換候補選択や文字列内のカーソル移動をPDF操作として横取りしない。
- メニュー、ポップアップ、モーダル、注釈エディター、候補ピッカーを優先する。入力先が不明なフォーカスにも入力側を譲る。
- 左右分割・上下分割のどちらでも、フォーカス中のPDFペインだけを操作する。
- Ctrl、Shift、Altなどの修飾キーを伴う矢印に、新しい割り当てを設けない。
- タブ、ペイン、表示モード、入力所有者、ウィンドウフォーカスの変更で長押しの所有状態を破棄する。その後に届くリピートは、新しい押下がない限り操作に使わない。

## 既存操作と座標の整合

PageUp／PageDown、ホイール、パン、自動スクロール、ツールバー、検索Enter、ズーム、IME、注釈の操作は維持する。メニューとツールバーの構築後に矢印の所有権を確認し、同フレームのPageUp／PageDown、ホイール、ポインター操作、明示的なパン、自動スクロールを優先する。同一フレームの二重処理を防ぐ。

連続表示のページ移動・アンカー復元は矢印より優先する。単ページ表示では復元した位置を開始位置として扱い、前フレームの遷移アンカーが残っていても押下・所有したリピートを受け付ける。端判定にはScrollArea処理前の位置を使い、平滑化スクロールがそのフレームで端に到達しても、押し始めから端だったとは扱わない。

ScrollAreaが確定した実際の可視サイズから上限を計算し、保存offsetと表示アンカーを同じ座標へ更新する。連続表示の矢印スクロールは選択ページの範囲に限定する。

現行実装の確認箇所：[src/app.rs](../../src/app.rs) の handle_arrow_navigation、central_panel、continuous_view、single_page_view、set_page_index、jump_to_single_page_edge、[src/app/navigation.rs](../../src/app/navigation.rs) の NavigationArrow、ArrowNavigationState、scroll_area_state_id、adjacent_page_index。

## 4方向の軸判定と長押し制御の検証記録（2026-10-24）

対象はmain HEAD `e5d51577258c6ae21336f684fac8af6ed7806b25` を基点とする作業ツリー。rootが対象checkoutと実際の `/workspace` bind mountの一致を確認したDev Containerで実行した。各Cargoコマンドは `docker exec -w /workspace 7bba2cfbb19b` に続けて実行した。テスト件数にはフィルター間の重複を含む。

| コマンド | 結果 |
| --- | --- |
| `cargo test --locked --bin lunapdf arrow_navigation`（実装前） | 追加した3件が失敗、既存12件が成功。横overflow、小ページの選択維持、軸ごとのfit判定の未対応を確認 |
| `cargo test --locked --bin lunapdf arrow_navigation`（実装後） | 14件成功。両表示モード・4方向のページ内移動、端停止、押し直し、移動先の端と直交軸維持、fit時のrepeat、ページサイズ混在、文書境界、小ページ複数可視での選択・idle安定、wheel／pan／既存jumpへの復帰、入力保護、分割ペイン、所有権・同frame競合を確認 |
| `cargo test --locked --bin lunapdf single_page` | 7件成功。単ページホイール、境界、ズーム、セッションアンカー |
| `cargo test --locked --bin lunapdf search_` | 15件成功。検索Enter／Shift+Enter、IME、検索完了状態など |
| `cargo test --locked --bin lunapdf page_input` | 4件成功 |
| `cargo test --locked --bin lunapdf display_mode_roundtrip` | 1件成功 |
| `cargo test --locked --bin lunapdf autoscroll` | 7件成功 |
| `cargo test --locked --bin lunapdf pan` | 15件成功 |
| `cargo check --locked` | 型・コンパイル検査成功 |
| `cargo fmt --all -- --check` | 整形検査成功 |
| `git diff --check` | 成功。whitespace errorなし。変更対象外の `.gitattributes` に関するCRLF正規化警告あり |

変更した仕様3文書の相対リンク54件について、参照先パスの存在を静的に確認した。

GUI、実OSのキーリピート／IME、Windows実機、配布物ビルドは未検証である。合成入力テストの成功を実機確認済みとは扱わない。以下の2026-10-22の記録は当時の旧仕様に対する結果であり、今回の受入の根拠とは分けて扱う。

## 前回実装時の検証記録（2026-10-22）

以下は今回の repeat 対応前に実施した検証であり、今回の変更を検証した結果ではない。

対象 checkout と `/workspace` の実 bind mount の一致を確認した Dev Container で、以下を実行して合格した。コマンドは `docker exec -w /workspace 7bba2cfbb19b` に続けて実行したものである。テスト件数にはフィルター間の重複を含む。

| コマンド | 結果 |
| --- | --- |
| `cargo test --locked --bin lunapdf arrow_navigation` | 12件成功。左右は新しい押下のみ、上下は所有したリピートをスクロールに使う旧挙動、端到達後の再押下、復元アンカー、平滑化スクロール残留、入力保護、左右／上下分割のクリックによるフォーカス、非表示時のフォーカス喪失、同フレーム競合を合成入力で検証 |
| `cargo test --locked --bin lunapdf single_page` | 既存7件成功。ホイール、境界、ズーム、セッションアンカー |
| `cargo test --locked --bin lunapdf search_` | 既存15件成功。検索 Enter／Shift+Enter、IME、検索完了状態など |
| `cargo test --locked --bin lunapdf page_input` | 既存4件成功 |
| `cargo test --locked --bin lunapdf display_mode_roundtrip` | 既存1件成功 |
| `cargo test --locked --bin lunapdf annotation_editor` | 既存14件成功 |
| `cargo test --locked --bin lunapdf autoscroll` | 既存7件成功 |
| `cargo test --locked --bin lunapdf pan` | 15件成功（今回追加した2件を含む） |
| `cargo check --locked` | 型・コンパイル検査成功 |
| `cargo fmt --all -- --check` | 整形検査成功 |
| `cargo clippy --locked --bin lunapdf --tests -- -D warnings` | 警告なしで成功 |

ホスト側の `git diff --check` も成功した。最初の左右キー回帰テストは実装前に失敗し、実装後に成功した。GUI、実際の OS キーリピート／IME、Windows 実機、配布物のビルドは今回の検証対象外であり、GUI未検証・実機未検証である。合成入力テストの成功を実機確認済みとは扱わない。

## 矢印キーリピート対応の検証記録（2026-10-22）

対象は main HEAD `0ff28ac2000390ad924445255128979150652aa5` を基点とする今回の作業ツリー。root が `/workspace` の bind mount と対象 checkout の一致を確認した Dev Container で、以下を実行した。各 Cargo コマンドは `docker exec -w /workspace 7bba2cfbb19b` に続けて実行した。

| コマンド | 結果 |
| --- | --- |
| `cargo test --locked --bin lunapdf arrow_navigation`（実装前） | 12件中7件成功・5件失敗。左右repeat、単ページfitの上下repeat、単ページ端からのrepeat遷移、平滑化スクロール後のrepeat遷移が未対応であることを確認 |
| `cargo test --locked --bin lunapdf arrow_navigation`（実装後） | 12件成功。左右両方向、連続表示の上下スクロール、単ページ端到達後のrepeat遷移、ページ全体が縦に収まる場合の上下1ページ移動、境界、repeat所有者変更、入力保護、分割ペイン、同フレーム重複を合成入力で確認 |
| `cargo test --locked --bin lunapdf single_page` | 7件成功。単ページホイール、境界、ズーム、セッション位置 |
| `cargo check --locked` | 型・コンパイル検査成功 |
| `cargo fmt --all -- --check` | 整形検査成功 |
| `git diff --check` | 成功。whitespace error なし。Git は変更していない `.gitattributes` の CRLF 正規化警告を出力 |

GUI、実 OS のキーリピート／IME、Windows 実機、配布物のビルドは未検証である。合成入力テストの成功を実機確認済みとは扱わない。
