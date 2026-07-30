# LunaPDF UI文字縦位置・フォントメトリクス追補修正 作業報告

## 1．作業範囲

作業開始時と終了時のGit状態は次のとおり。

| 項目 | 開始時 | 終了時 |
| --- | --- | --- |
| ブランチ | `main` | `main` |
| HEAD | `3628df4e21dc6b15abefc922b8eda13bdcef5de7` | `3628df4e21dc6b15abefc922b8eda13bdcef5de7` |
| 先頭コミット | `3628df4 fix: PDFカーソルを標準表示へ統一` | 同左 |
| 未コミット差分 | なし | 本報告記載の差分のみ |

指示書に従い、commit、push、PR作成は行っていない。

変更ファイルは次のとおり。

- `src/ui/fonts.rs`
- `src/app.rs`
- `docs/tasks/README.md`
- `docs/tasks/ui-improvement/260730_lunapdf_codex_font_vertical_alignment_followup_instructions.md`
- `docs/tasks/ui-improvement/260730_lunapdf_font_vertical_alignment_followup_report.md`

`src/ui/sidebar.rs`、PDF本文のフォント処理、依存関係、タブとツールバーの高さ定数、クリック領域は変更していない。最終的なメイリオ選択では行メトリクスによりメニューとツールバーの表示高が小さくなるが、ユーザーの明示的な許容により、高さを戻す追加レイアウト修正は行っていない。

## 2．修正前の確認

### 実際に選択されたフォント

Windows 10 `10.0.19045` 上では、既存探索順の先頭である次のファイルが読み込まれた。

```text
C:\WINDOWS\Fonts\YuGothR.ttc
```

前回の`+1.0 logical pt`デバッグビルドの起動時出力は次のとおり。

```text
[旧実行ログ] [lunapdf] CJK UI font: C:\WINDOWS\Fonts\YuGothR.ttc; y-offset: 1.0 logical pt
```

`+1.5 logical pt`へ更新して再ビルドしたデバッグ版の起動時出力は次のとおり。

```text
[lunapdf] CJK UI font: C:\WINDOWS\Fonts\YuGothR.ttc; y-offset: 1.5 logical pt
```

最終候補順へ更新した標準デバッグ版の起動時出力は次のとおり。

```text
[lunapdf] CJK UI font: C:\WINDOWS\Fonts\meiryo.ttc; y-offset: 0.0 logical pt
```

同じホストで、`YuGothR.ttc`、`YuGothM.ttc`、`meiryo.ttc`、`meiryob.ttc`がすべて存在し、読み取り可能であることを確認した。最終候補順では`meiryo.ttc`が補正なしで選択される。

Proportionalでは読み込んだフォントを`Highest`で登録する。補正値が`0.0`の候補では同じ`FontInsert`にMonospaceの`Lowest`も関連付け、補正が必要な候補では別名のMonospace登録を追加する。游ゴシックが日本語とASCIIの両方のglyphを持つため、計測した日本語、半角英数字、混在文字列はいずれも同じProportional登録フォントでレイアウトされた。

### galleyとglyph meshの中心差

Windows上の`egui 0.35.0`で14ptの実フォントを読み、次の文字列を同じProportionalフォントでレイアウトした。

- `ファイル`
- `日本語ABC123`
- `1 / 12ページ`
- `検索 (Ctrl+F)`
- `長い日本語ファイル名.pdf`
- `ABC123`

galley矩形中心に対するtight glyph mesh中心の平均差は次のとおり。負値はglyph meshが上側にあることを示す。

| フォント | pixels/point | 補正なし | `+0.5pt` | `+1.0pt` | `+1.5pt` |
| --- | ---: | ---: | ---: | ---: | ---: |
| YuGothR | 1.00 | -4.583 | -3.750 | -3.583 | -2.583 |
| YuGothR | 1.25 | -4.670 | -3.870 | -3.870 | -3.070 |
| YuGothR | 1.50 | -3.828 | -3.161 | -2.495 | -2.495 |
| YuGothR | 2.00 | -3.917 | -3.417 | -2.917 | -2.417 |
| meiryo | 1.00 | -0.167 | +0.833 | +0.833 | +1.833 |
| meiryo | 1.25 | -0.873 | -0.073 | -0.073 | +0.727 |
| meiryo | 1.50 | -0.394 | +0.273 | +0.606 | +0.939 |
| meiryo | 2.00 | -0.292 | +0.208 | +0.708 | +1.208 |

`YuGothM.ttc`は`YuGothR.ttc`と同傾向、`meiryob.ttc`は`meiryo.ttc`と同傾向だった。游ゴシックには下方向補正が必要だが、メイリオは補正なしですでに中心に近く、正補正によって100%表示で下寄りになる。

### TextEdit

`egui 0.35.0`の実装を確認し、`FontTweak::y_offset`は正値でglyphを下方向へ論理pt単位で移動し、galleyのレイアウト寸法自体は変更しないことを確認した。

同じ実装では、`TextEdit`の既定値は`Align2::LEFT_TOP`である。ツールバーの`horizontal_centered`は24ptのTextEdit外矩形を行中央へ置くが、TextEdit内のgalleyは上揃えのままだった。明示APIとして`TextEdit::vertical_align(Align::Center)`が利用できることも確認した。

## 3．確定した原因

タブタイトルとハイライト一覧では、既存コードがgalley矩形を各テキスト矩形の幾何学的中央へ配置していた。ツールバーも固定高行の中央へ各ウィジェットを配置していた。これらの既存レイアウトは正しく、変更理由はなかった。

残っていた差は次の二点で説明できる。

1. 游ゴシックのglyph meshの視覚中心がgalley矩形中心より上側にある。
2. 単一行TextEditだけは、固定高の外矩形内で内部galleyが既定の上揃えになっている。

原因確認はソース、実フォント、実際のeguiレイアウト・tessellation結果による。tight mesh境界は人間の知覚そのものではないため、数値だけをピクセル完全一致の受入判定には使用していない。

## 4．採用した修正

### フォント登録

Windows候補を`(ファイル名, Y補正値)`として関連付け、読み込まれた候補の値を`FontData`へ`FontTweak::y_offset`で設定した。補正値が`0.0`の候補は一つの`FontData`を両familyで共有し、非ゼロの候補だけProportionalとMonospaceを別の`FontData`に分ける。

| Windows候補 | Y補正値 |
| --- | ---: |
| `meiryo.ttc` | `0.0` |
| `meiryob.ttc` | `0.0` |
| `YuGothR.ttc` | `+1.5 logical pt` |
| `YuGothM.ttc` | `+1.5 logical pt` |

游ゴシックでは計測上`+1.0pt`が`+0.5pt`より150%と200%で安定して改善したが、後述の拡大画像で`+1.0pt`でも一覧文字が上寄りと判明したため、フォールバック時は比較済み次候補の`+1.5pt`を使う。最終的に先頭候補としたメイリオは、補正なしを維持した。

追加調査では、`YuGothR.ttc`のface 0が`Yu Gothic Regular`、face 1が`Yu Gothic UI Semilight`であることを確認した。UI用face 1は補正なしでtight glyph mesh中心がgalley中心より下側になり、通常face 0との単純な差し替えには適さなかった。`meiryo.ttc`の通常face 0は補正なしで中心に近く、後述の比較画像でもタブ、ツールバー、注釈一覧が自然に見えたため、Windowsの先頭候補へ変更した。

LinuxとmacOSは実機確認していないため、既存候補を維持し、補正値を`0.0`に限定した。

フォントファイルは一度だけ読み込む。補正値が`0.0`の候補では一つの`FontInsert`にProportionalの`Highest`とMonospaceの`Lowest`を登録し、同一設定の重複登録とバイト列の複製を避ける。非ゼロの候補では、`FontTweak`がFontData単位のAPIであるため、Proportionalの`Highest`に候補の補正値を設定した登録と、Monospaceの`Lowest`に`0.0`を設定した別名登録を各一度だけ作る。

### TextEdit

ページ番号欄と検索欄に共通の小さなbuilder helperを使い、`vertical_align(Align::Center)`を明示した。これはTextEdit内でgalleyを中央へ置く設定であり、固定Y座標を加える処理ではない。

FontTweakはgalley内のglyph meshだけを移動し、TextEditの設定はgalleyをclip矩形内へ配置する。このため、同じ座標補正を二重に加えていない。

複数行の注釈コメント欄は上から入力する既存挙動が必要なため変更していない。

## 5．自動テスト

### Dev Container

| コマンド | 結果 |
| --- | --- |
| `cargo fmt --check` | 成功 |
| `cargo check` | 成功 |
| `cargo test` | 224成功、0失敗、既存ignored 2 |
| `cargo check --release` | 成功 |
| `cargo clippy --all-targets` | 成功、警告なし |

追加・更新したテストでは次を確認した。

- OS別候補パスが重複せず、既存候補を維持する。
- Windowsでは大文字・小文字だけが異なるフォントディレクトリを重複候補にしない。
- 非Windows候補に未検証補正を適用しない。
- Windowsの游ゴシックとメイリオへ意図した候補別補正値を対応させる。
- Windowsでは実測と実機画像で中央に近かったメイリオを先頭候補にする。
- 候補が存在しない場合と空の場合に次候補へ進む。
- 一つも読み取れない場合に`None`で非致命に終了する。
- 補正値`0.0`では一つのFontInsertに二つのfamilyを登録し、非ゼロでは別名のFontInsertを二つ作って各一つのfamily、priority、offsetを検証する。
- TextEditのgalley中心とclip矩形中心が一致する。
- 既存のタブ領域、ハイライト行中央、行Response、TextEditコピーなどの回帰テストを維持する。

### Windows GNU

| コマンドまたは対象 | 結果 |
| --- | --- |
| debug cross build | 成功、`dist/lunapdf-debug.exe`作成 |
| release cross build | 成功、`dist/lunapdf-release.exe`作成 |
| Windows test `--no-run` | 成功 |
| Windowsフォントテスト7件 | 7成功 |
| Windows TextEdit中央揃えテスト1件 | 1成功 |

Windows testの途中版を初回実行した際は、ホストの`TEMP/TMP`がアクセス不能な`T:\Temp`を指していたため、`tempfile`を使う2件が製品コード到達前に失敗した。`TEMP/TMP`を`target/windows-test-temp`へ限定すると当時の6件が成功した。最終版では同じ一時ディレクトリ指定で、候補順テストを含む7件すべてが成功した。

Windows全テストも実行し、225成功、2失敗、既存ignored 2だった。失敗は今回未変更の`pdf::mupdf_backend`にある次の2件で、Windowsメタデータを保持した一時PDF置換が`AccessDenied`になった。

- `persist_callback_runs_only_after_current_version_check_succeeds`
- `redacted_pdf_accepts_highlight_and_is_saved_by_full_rewrite`

同じテストはDev Containerの全テストで成功した。今回のフォント・TextEdit変更による失敗ではないが、Windows全テスト成功とは扱っていない。

## 6．Windows実機確認

`+1.5 logical pt`を適用したWindows GNUデバッグビルドを`acceptance-highlight-260728-r2.pdf`付きで起動し、次を確認した。

- アプリが起動を継続する。
- 実際の選択フォントが`YuGothR.ttc`である。
- 適用補正値が`+1.5 logical pt`である。
- 日本語のみ、半角英数字のみ、混在、括弧、長い日本語ファイル名、ページ数表記を実フォントで計測した。
- 100%、125%、150%、200%相当のpixels/pointで、補正が同じ下方向へ作用し、galley寸法を変えない。
- TextEditのgalleyが24pt相当のclip矩形中央へ配置される。

ただし、Windows画面取得はComputer Useの`SetIsBorderRequired`がこのWindows 10環境で`0x80004002`を返し、再試行も失敗した。プロジェクトの検証メモに従い、OS全画面キャプチャへ切り替えていない。

2026-07-30に、ユーザーから変更後LunaPDFで`lunapdf-ui-followup.pdf`を開いた1200×960のライトテーマ画像が提供された。当初はこの画像に基づき主要UIを合格と判定したが、修正後に提供された拡大画像で`+1.0pt`でも一覧文字が色スウォッチと行中心より上に残り、同じ傾向がタブとツールバーにもあることが判明したため、前回の合格判定を撤回した。比較済みの次候補`+1.5 logical pt`を採用し、補正値を更新した。

前回の`+1.0 logical pt`画像について確認できた項目と問題は次のとおりだった。

- 一覧の日本語・半角英数字を含む省略表示は、色スウォッチと行中心より上寄りに残っていた。
- 同じ上寄りの傾向がタブとツールバーにも見られ、前回の主要UI合格判定を撤回した。
- ページ番号`1`、ページ数`/ 1`、ズーム`285%`、検索件数`0 / 0`、検索placeholderに上端・下端のクリップはなく、タブ・ツールバー・サイドバー行の高さと要素の重なりは維持されていた。

その後、ユーザーから`+1.5 logical pt`版の1200×934ライトテーマ画像が2枚提供された。アウトライン表示と注釈一覧表示を切り替えた画像で、次を確認した。

- タブタイトルはタブ領域の上下中央に見える。
- ツールバーは`+1.0 logical pt`版より改善したが、ユーザー目視では通常ラベルと数字にわずかな上寄りが残った。
- ページ番号入力欄、検索欄、ページ数、ズーム倍率、検索件数に上端・下端のクリップや明確な下寄りはない。
- 「目次はありません」は、通常リスト内で不自然に上寄りになっていない。
- 注釈一覧の「1ページ LunaPDF 日本語…」は、日本語と半角英数字のベースラインが分離せず、色スウォッチの中心と概ね揃って見える。
- `+1.0 logical pt`画像と同じ座標範囲を比較すると、タブ、ツールバー、ページ数、注釈一覧の文字描画境界はいずれも1px下へ移動していた。フォント補正が対象UIへ一貫して作用している。
- タブ、ツールバー、サイドバー行の高さ、アイコン位置、区切り線、テキスト省略は維持され、要素の重なりはない。

この画像により、実際に選択された`YuGothR.ttc`を使うWindows実機で、`+1.0 logical pt`に残っていた上寄りが`+1.5 logical pt`で改善したことを確認した。ただし、ツールバーの完全な見かけ上の中央は確認できていない。画像の画面全体に対するDPI条件は特定できないため、各DPI条件の目視確認とも扱わない。

補正値をさらに増やす前に、実測で補正なしの中心位置が良かった`meiryo.ttc`を先頭候補とする比較版を作成した。ユーザー提供の1200×934ライトテーマ画像では、タブ、ツールバー、注釈一覧の字面が自然な中央に見え、日本語と半角英数字のベースライン分離、クリップ、明確な下寄りはなかった。

同じ境界座標の比較では、メイリオの21ptのgalley高さにより、游ゴシック版よりメニューバーの表示高が1px、ツールバーの表示高が2px小さくなった。ユーザーはこの縮小を許容し、高さを戻す追加修正は不要と明示したため、メイリオ優先案を採用した。高さ定数やレイアウトコードは変更していない。

ツールバーの縮小後外寸30ptは、既存の24pt操作領域、`Frame::side_top_panel`の上下2pt内側余白、上下1pt枠から導かれる。游ゴシック版の32ptは22ptの行ボックスが24pt操作領域を超えてTextEditを押し広げた結果だった。旧外寸へ戻すには追加の余白または固定値が必要になり、文字中央位置の改善には寄与しないため採用していない。

文字列のtight mesh中心を個別に合わせる方式は、数字、括弧、descenderを含む英字など内容が変わるたびに表示位置が上下するため採用していない。文字サイズ比例の`FontTweak::y_offset_factor`も、タブとツールバーが同じ14pt系であるため、両者の見かけの差を解消しない。最終選択のメイリオは`y_offset`と`y_offset_factor`のどちらも使わず、フォント自体のメトリクスを利用する。

画像に表示されていない、または画像だけでは条件を特定できない次の項目は未確認であり、成功扱いしない。

- 注釈編集、コンテキストメニュー、ツールチップ、エラー・状態表示の見かけ上の中央。
- 日本語IME操作中の見かけ。
- ダークテーマの見かけ。
- 提供画像の実ディスプレイDPIは特定できない。従って、実ディスプレイ設定100%、125%、150%、200%それぞれの目視は未確認である。上記DPI結果はeguiへpixels/pointを設定したメッシュ計測である。
- 未保存マーカー付きタブと実際の長い日本語ファイル名タブの目視。
- 入力済み検索文字列、検索結果が存在する場合の件数表示、クリック領域、カーソル挙動の手動確認。

## 7．未確認範囲と残課題

- LinuxとmacOSの実機表示は未確認であり、補正していない。
- Windowsの最終構成で選択されるのは`meiryo.ttc`。`YuGothR.ttc`は途中ビルドの実機目視まで、`YuGothM.ttc`と`meiryob.ttc`は実フォントのメトリクス計測までである。
- Windows 11、複数モニター間DPI切替、日本語IME変換中の目視は未確認。
- メイリオ選択によりメニューバーの表示高が1px、ツールバーの表示高が2px小さくなる。ユーザーが許容した既知の変更であり、高さを戻す処理は追加していない。
- 上記の非表示項目とDPI条件は追加の人間による画面確認が必要である。

## 8．差分監査

- UI要素ごとの固定Y座標補正は追加していない。
- タブ、ハイライト一覧、ツールバーラベルの既存中央配置は変更していない。
- タブ、ツールバー、行の高さ定数とクリック領域の実装は変更していない。フォント行メトリクスによるメニューとツールバーの表示高縮小は、ユーザー確認済みである。
- PDF本文、レンダリング、注釈操作、フォントファイル、依存関係は変更していない。
- fallback、互換wrapper、実行時設定画面、外部依存を追加していない。
- commit、push、PR作成を行っていない。
