# LunaPDF CI失敗原因とテスト品質の監査・整理報告

## 結論

基準 commit `48e8cd03` の CI では、387 件が成功し、外部変更との競合を再読込する1件が失敗、2件が既存の ignored でした。失敗原因は本番処理ではなく、テストfixtureが同じ長さ・更新時刻のPDFを続けて上書きし、`DocumentVersion` が変わるという前提を満たさなかったことです。Task 1 のfixture修正 commit `a8b457a11723b85d14a31040180fbab5bb726e3e` では、競合版と最新外部版に異なる有効PDFを使い、更新時刻を同じに固定してfixtureの前提を明示しました。

Task 2 では、仕様にない内部値を期待値としてコピーしていたテストだけを振る舞い中心に整理しました。監査で見つけたdiagnostics testの非同期fixture境界も修正しました。変更はテスト部分に限り、テスト宣言数の削減、仕様変更、本番コード変更はありません。変更対象テストはすべてDev Containerで成功しました。

## CI失敗の再現と範囲

旧fixtureで同一の `DocumentVersion` を強制するRED確認では、identity・length・modified が一致し、CIと同じ比較で失敗しました。修正後は `conflicted_reload` の6ケースを各20回、合計120回実行してすべて成功し、`reload` の16件も成功しました。これはfixtureの再現性確認であり、metadataを使う現行検出方式が内容だけの変更を完全に検出する証明ではありません。

同じidentity・length・modifiedのままPDF内容だけが変化した場合、現行実装はその変化を識別しません。現行仕様に内容hashによる完全検出の約束はなく、今回その保証を追加していません。この制約は既存挙動として区別し、完全検出を保証する表現は採用しません。

Task 2で最初に実行したLinux debug full suiteは380件中377件成功、2件ignored、1件failedでした。失敗は `diagnostics_keeps_visible_render_failure_when_two_results_are_drained_together` です。原因はテストのevent barrierでした。`finish_async_document_open` は `Opened` を処理した時点で戻り、その処理が非同期の `LoadOutline` を要求します。ところが従来の1回の追加 `receive_document_events` はoutline完了を保証せず、`diagnostics_snapshot().event` はworker eventの型ではなくqueue lengthを数えていました。そのため `baseline + 2` は2件のrender結果が揃った証拠にならず、successful tileを一度のdrain後に確認できない状態を作れます。失敗時に実際にqueueされていたevent型のtraceは採取していないため、個別のevent順序を観測事実としては記しません。

前修正の再現では、同じbinaryによる対象testの単独50回はすべて成功し、`diagnostics_` groupを8 threadsで50回実行すると13回目に11件中1件が同じassertで失敗しました。修正では両testとも既存の5秒deadline内で `outline_in_flight == false` と `outline.is_some()` を受信・確認してからbaselineを取り、render要求後の結果eventを数えます。これによりstartup outline eventをrender結果数へ混ぜず、成功・失敗結果がqueueに揃った後に一度だけdrainする意図を保ちます。production側のfailure state保持と、成功tileがpending時だけ成功を設定するguardは変更していません。

## テスト全体の棚卸し

監査ではRustのテスト宣言393件、22ファイルを実装経路と現行仕様に照合しました。cfg適用後のdebug件数はWindowsが390件（実行対象388、ignored 2）、Linuxが380件（実行対象378、ignored 2）です。debug限定14件（app 12、worker 1、diagnostics 1）を除くrelease宣言数はWindows 376、Linux 366です。ignoredは性能matrix出力と外部viewer用PDF出力で、通常の回帰テストを抑止するものではありません。

| 領域 | 宣言数 | 主に保護する振る舞い |
|---|---:|---|
| app tests | 187 | 非同期結果の世代・revision・可視範囲、保存・外部更新・終了、入力所有権、検索、復元、zoom・scroll |
| app workspace | 6 | 分割領域の被覆・非重複・最小サイズ、ドラッグ挿入位置 |
| diagnostics | 1 | 診断TSV各行の列構成 |
| domain | 43 | 注釈hit-test、RGBA容量、selection geometry、schema migration、tab/split/focus |
| UI | 41 | 注釈patch/draft、font face、image source、一覧操作、click/drag/selection座標 |
| render | 14 | cache予算・順序・overflow、page配置・anchor、tile境界とMuPDF丸め |
| PDF backend | 45 | 実PDFの保存・再読込・Undo・権限・外部版保護、描画・検索・注釈snapshot |
| worker | 16 | 優先度、cancel・世代、実workerからの成功・失敗event |
| print layout | 12 | 比率・向き・範囲・strip被覆・memory budget |
| session store | 9 | roundtrip、不正・旧schema、51 tabs、atomic save、OS保存先 |
| Windows platform | 19 | OS association、UTF-16 IPC、ACK、user照合、並行要求 |
| **合計** | **393** | |

Rust以外に独立したテストsuiteやtest scriptはありません。distribution workflowの64行目はinstallerとportable ZIPを各1件検査します。packaging scriptの `throw` は製品入力・成果物の検査です。Cargoは単一binaryで、独立integration test、library doctest用library、追加frameworkはありません。

件数だけで重複とは判断しませんでした。session schema migration、`LunaPDF.Document.1`、IPC magic/ACK/UTF-16制限、OS URI、wheel設定25–300%とdefault 100%、recent colors 5件は永続形式・protocol・現行仕様です。51 tabsのdomain/store/startup、selection preview/savequad/backend/viewport、worker世代/UI revision/visibility/queue到達確認、PDF保存時の注釈・xref・Undo・外部bytes・temp file・Windows属性、diagnosticsの列対応と行形式、external reloadのcancel/success/failure/close/switch/saving/suspended各遷移も異なる境界を守るため維持します。

## 固定値候補の判定

| 判定 | 対象と変更・維持した保証 |
|---|---|
| 変更 | `repaint_delay_policy_returns_to_idle_after_work_completes` と `hidden_suspended_resume_candidate_is_not_active_until_reopened` は33ms/300ms/1sを外し、delayが正で、active < external wait < idleの優先順位、作業完了後のidle復帰を確認します。hidden/suspended状態の待機中と再open後のactive判定も維持しました。 |
| 変更 | page inputの列・幅テストは固定3/4/5桁を外しました。短文書fixtureの桁境界は現在の `PAGE_INPUT_MINIMUM_COLUMNS` から生成し、その数値自体は固定していません。toolbar幅の安定、ページ番号の必要桁の収容、長文書での幅拡張と文字の非clipを確認します。stable toolbar columnの意図は保っています。 |
| 変更 | `fit_resize_coalesces_to_the_latest_zoom_before_advancing_generation` は124/174/175msのコピーを外し、現在のdebounce期間からdeadline直前・deadlineを作ります。最新zoomへのcoalescingとgenerationが一度だけ進む保証は維持しました。 |
| 変更 | search clearテストからfield幅180の一致だけを除きました。clear targetがfield内の右側にあり、実クリック、focus、古い結果の拒否を含む既存保証は残しています。 |
| 変更 | autoscrollは4800という上限値コピーを除き、dead zone、方向、距離に伴う速度増加、現在の上限を越えないことを確認します。 |
| 変更 | wheel speed startupテストはnon-Windowsの20/80を除き、実Contextの既定速度に対する設定倍率、設定適用と保存を確認します。Windowsのnative wheel-line倍率検証は維持しました。 |
| 変更 | pointer jitterと隣接するshort dragはtest fixtureでclick thresholdを明示します。threshold未満の微動では選択せず、thresholdを越える意図した短いdragでは1 glyphを選ぶ保証を残しました。 |
| 変更 | arrow overflow holdの±0.02とwheel/pan restorationのanchor.y≈0.55を除きました。hold中のframe数も固定せず、初回の実移動量とfixture page寸法から端を覆う有限回数を作ります。要求方向への移動、直交軸、境界、長押し中の非跨ぎ、idle安定、再押下で一度だけ跨ぐ挙動、wheel/pan優先と復元後の方向・安定性は維持しました。現行keyboard仕様は距離を固定しません。 |
| 変更 | `render/layout.rs` の最初の3テストは、暗黙にPAGE_GAPを焼き込んだ配置・viewport座標を、明示fixture寸法と既存PAGE_GAPから計算します。prefetch対象はページ中央のviewportと1 page strideから構成します。可変ページ高の累積、水平中央配置、total height、可視/prefetch境界、gap上の座標を次pageへ割り当てる挙動を維持しました。 |
| 変更 | 二つのdiagnostics testは、startup `Opened` 後の非同期outlineがterminal stateとなりoutlineを受信するまで既存deadline内で待ち、baselineを取ってからrender requestを送ります。render失敗を成功結果が上書きしないこと、成功tileの保持、pending消去、Warmup/Startupとdiagnosticsログ出力、およびstale prefetch失敗を現在のvisible requestに誤適用しないことを維持しました。 |
| 維持 | `color_palette_contains_ten_labeled_presets` の10件は仕様上の固定数ではありませんが、現在の2行×5列menuが全indexへ到達し、不足時のpanicや未表示presetを検出するguardです。palette/gridを変える時に機能testと一緒に更新する対象として維持しました。 |
| 維持 | `every_toolbar_icon_has_embedded_bytes_and_a_stable_asset_uri` は画像源の欠落とURI衝突を防ぐため維持しました。bytes variantやURI prefix/extensionは内部表現ですが、代替の可用性・一意性検証を追加せずに緩和する根拠はありません。 |

close iconの等長vector X（導入 `de57d64`）、toolbar textのclip内垂直中央（`33c84fd`）、light/dark両themeでのwarning可読性（`f933513`）、app errorとactive document errorの個別dismiss（`application.md` §10）は実回帰または現行仕様に対応するため変更していません。不要なテスト削除・統合はしていません。

## 仕様との対応

現行仕様は変更していません。page input/zoom/debounce/arrow移動は [application.md §4](../../specifications/application.md)、autoscroll/wheelは §5、search clearは §6、selection dragは §7、wheel設定とrestoreは §9、error dismissは §10に対応します。[keyboard-navigation.md](../../specifications/keyboard-navigation.md) は操作軸、方向、hold中の境界と再押下の規則を定め、1回あたりの移動距離を固定しません。今回のテストはその既存契約に沿い、距離・時刻・座標の内部コピーだけを外しています。

## 検証と残る制約

変更に関係するtest filterで20件を実行し、すべて成功しました。Task 2で直接変更したのは17件（app 12、render layout 3、viewport 2）です。filter結果にはpage input 4件（変更2件）、layout 4件（変更3件）など未変更の隣接テストも含みます。root reviewで修正したpage input/layout fixtureもmodule filterで再実行して成功しました。diagnostics対象test単独50/50、`diagnostics_` group 8-thread反復50/50も成功しました。Dev Containerの実体は `lunapdf_devcontainer`、workspace `7bba2cfbb19b`（`/workspace` は対象checkoutをbind）で、rustc 1.99.1 (`58cf8f9c3`, 2026-10-13)、cargo 1.99.1 (`5f94df478`, 2026-08-27)です。個別実行コマンドの形は `docker exec -w /workspace 7bba2cfbb19b cargo test --locked --bin lunapdf <filter>`、反復は同containerのtest binary `target/debug/deps/lunapdf-67ab095f6c3d5f69` を使いました。`cargo fmt --all -- --check` と `git diff --check` も成功しました。

修正前のLinux debug full suiteは380件中377件成功、2件ignored、1件failedでした。失敗原因は上記の非同期fixture境界です。修正後の `cargo test --locked` は380件中378 passed、0 failed、2 ignoredでした。`cargo clippy --locked --all-targets` もexit 0で警告出力なしです。

CIと同じRust 1.92.0は未導入です。Windows hostはstable 1.96.0です。Windows `cargo fmt --check` は成功しました。Windows test試行はMuPDF native C build後、bindgenが必要とする `libclang.dll` 不在でtest開始前に停止しました。依存build.rsは再試行ごとにnative outputを消して再buildするため、同じ環境でfull test/clippyを繰り返しても製品のevidenceは増えません。Windows full test、Windows Clippy、Windows 2022 / Rust 1.92.0のCI実機は未確認で、成功とは扱いません。GUI/OS入力も今回の受入対象ではなく未検証です。
