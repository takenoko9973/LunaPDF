# LunaPDF タブ並べ替え・左右分割ビュー 作業報告

## 1．作業開始時点

- ブランチ: `main`
- 開始コミット: `16856b696ebb04ee3cc5e47bd54d0fd1a521c29f`
- 開始時の先頭: `16856b6 docs: リファクタリング計画と実施結果を記録`
- `origin/main`: `3df694f865b2f1978ccc94536c6e9b5a4fdcfa11`
- 差分関係: ローカル `main` が `origin/main` より1コミット先行し、後退は0件だった。
- 未コミット差分: なし。
- 適用した作業指示: ルートの `AGENTS.md`。対象ディレクトリに追加の `AGENTS.md` はなかった。
- 検証上の注意: `.codex/validation-lessons.md` を確認した。GUI受入ではOS全画面キャプチャやシステムクリップボードを使用せず、対象ウィンドウとアプリ状態に限定して観測する。
- 変更前の検証結果:
  - `cargo fmt --check`: 成功。
  - `cargo check`: 成功。
  - `cargo test`: 225 passed、2 ignored、失敗0件。
  - `cargo check --release`: 成功。
  - `cargo clippy --all-targets -- -D warnings`: 成功。
  - 初回は Docker Desktop が停止しており製品コード到達前に失敗した。Docker Desktop と `workspace` サービスの起動後に全項目を再実行し、上記結果を得た。

## 2．調査結果

### 2.1 タブ、文書、表示状態の所有関係

- `TabState` は正規化済みパスの `Vec<Tab>` と、唯一の選択位置 `Option<usize>` を持つ。
- `PrototypeApp` は `TabState` と別に `Vec<DocumentTab>` を持ち、同じ位置インデックスで暗黙に対応させている。追加は両方の末尾、終了は両方の同じ位置を削除することで整合させている。
- `DocumentTab` は単調発行の `document_id`、PDF worker、PDF情報、表示状態、検索、選択、注釈ページ、編集履歴、dirty、保存・印刷中状態、タイル、サムネイル、エラーを所有する。
- `ViewState` は `DocumentTab` ごとにページ位置、倍率、表示モード、スクロール復元、autoscroll、render generationを持つ。
- `PageViewport` はアプリ全体に1個だけあり、選択ドラッグ、空白パン、右クリック対象などの入力途中状態を持つ。

### 2.2 インデックス依存と最初に変更する境界

- タブ描画、選択、終了、セッション保存は `tabs[index]` と `documents[index]` の対応を前提にする。
- `active_index()` は唯一の選択位置を返し、共通ツールバー、サイドバー、ショートカット、中央表示、注釈UIの対象を兼ねる。
- 非同期 Tile、Text snapshot、Annotation page は documentの generation・revision・要求キーに加え、`active_index()` と一致するときだけUI結果として受理する。
- 非アクティブ文書の休止も `active_index()` 以外を候補にするため、分割後の反対側表示文書を除外できない。

このため、最初に変更する境界は、文書実体を位置で並べ替える処理ではなく、安定したタブIDで「表示順・所属ペイン・各ペインの選択」と文書実体を関連付ける境界とする。

### 2.3 非同期、キャッシュ、編集の不変条件

- `document_id` はGPU／thumbnail cache、注釈editor／picker、eguiのページ・検索入力IDに使用され、表示位置とは独立している。
- Tile cache keyは document ID、ページ、倍率、DPI、回転、tile spec、revisionを含む。完了時にはrender generation、revision、wanted setも照合する。
- Text snapshot、annotation page、search、selection、thumbnail、highlight indexには個別のrevisionまたはgenerationがある。
- Undo履歴、pending edit、dirty、保存・印刷中状態は `DocumentTab` に属するため、タブ移動で `DocumentTab` を作り直してはならない。
- 注釈editor／pickerは document IDへ束縛されており、ペイン移動後も対象文書を維持できる。表示先を判定するときだけ所属ペインを引く必要がある。

### 2.4 セッション、終了、休止

- 変更前のschema 1は `tabs` の順序と唯一の `selected_tab` を保存し、ペイン所属、各ペイン選択、フォーカスペイン、分割方向・比率を表現できない。
- 終了確認は主に正規パス、注釈UIとcache除去は document IDを使う。保存完了後の終了対象はパスで追跡するため、表示順変更の影響を受けない。
- 休止候補は512 MiB超過時、唯一のactive以外から最終利用時刻が古いclean文書を選ぶ。分割後はactiveではなく可視Tab ID集合を除外条件にする必要がある。

### 2.5 使用中のUI API

- `eframe`、`egui`、`egui_extras` は0.35.0。
- タブの選択領域には `Sense::click_and_drag` と `Response::drag_started_by`／`dragged_by`／`drag_stopped_by` を利用できる。
- 現行タブIDは表示indexをsaltにしているため、安定Tab IDへ変更する。
- 閉じる領域は選択領域と矩形が分離されているためクリック専用のまま維持し、閉じる領域からドラッグを開始させない。

## 3．実装計画

### 3.1 検討した案

1. `TabState.tabs` と `documents` を同じ表示順で並べ替え続ける案。
   - 既存差分は小さいが、すべての並べ替え、ペイン移動、復元、終了で二つの可変配列の同期が必要となる。位置index依存を残すため採用しない。
2. `DocumentTab` をペインごとに移動し、ペインが文書実体を直接所有する案。
   - 同一性は明確になるが、非同期event受信、全タブ終了、cache予算、保存・印刷処理の列挙を広く作り直す必要があるため採用しない。
3. 文書配列を開いた順の安定レジストリとして維持し、安定Tab IDでペイン配置を管理する案。
   - 並べ替えとペイン移動がワーカー、編集履歴、cacheを動かさず、既存の文書event走査も維持できるため採用する。

### 3.2 採用する状態

- `TabId`: pathや表示位置と独立した、実行中に再利用しない識別子。
- `PaneId`: ペイン固有の入力状態とegui IDを識別する。左右という名前を持たせない。
- タブ配置: 最大2個のペインごとにTab IDの表示順と選択Tab IDを持つ。
- フォーカス: 共通UIの操作対象を決めるfocused Pane IDを1件持つ。
- PDF操作状態: Pane IDごとの `PageViewport` とし、別ペインの選択、パン、右クリック状態を混在させない。
- 分割表現: 初期実装は水平軸だけを使用するが、保存形式にはaxisを持たせ、将来は上下用のlayout計算とdrop領域を追加できるようにする。任意個ペインや分割木は導入しない。

### 3.3 移行順

1. 単一ペインのままTab ID、Pane ID、IDから文書indexを引く境界へ移行する。
2. タブの表示順だけを変更する状態遷移と回帰テストを追加する。
3. PageViewportをペイン別にし、focusedとvisibleの判定を分離する。
4. 従来位置の上部タブバーでペイン所属を区切り、左右PDF表示、drop、セパレーターを追加する。
5. 非同期結果受理、休止、注釈UI、終了処理をvisible Tab ID集合へ統合する。
6. schema 2へペイン配置を保存し、schema 1を単一ペインへ変換して読み込む。

### 3.4 テスト方針

- タブ順、選択維持、分割作成、ペイン間移動、空ペイン縮約、最大2ペイン、51件以上をUI非依存の状態遷移として検証する。
- drop位置、分割矩形、比率clampを純粋関数として検証する。
- 非同期結果はfocusedではなくvisible文書で受理し、非表示・古いgeneration・古いrevisionを拒否することを検証する。
- schema 1変換、schema 2の重複所属・空ペイン・不正選択・不正比率を検証する。
- 既存の単一ペインテストを維持し、全体検証後にWindows buildと可能なGUI受入を行う。

## 4．実装結果

### 4.1 変更ファイル

- `src/domain/tabs.rs`
  - 安定した`TabId`、`PaneId`、最大2ペインの配置・選択・フォーカス・分割比率を追加した。
  - 同一ペイン内reorder、単一ペインからのsplit、ペイン間move、空ペイン縮約、セッションlayout復元を状態遷移として実装した。
- `src/app/workspace.rs`
  - 左右ペインとセパレーターの矩形、タブ挿入位置、左右drop候補をUIから分離した純粋関数として追加した。
- `src/app.rs`
  - 従来位置の上部タブバー、左右PDF表示、D&D、セパレーター、focused／visibleの分離、ペイン別`PageViewport`、セッション統合を実装した。
- `src/app/events.rs`
  - Tile、Text snapshot、Annotation pageの完了受理をfocused 1件ではなくvisible文書集合へ変更した。
- `src/domain/session.rs`、`src/persistence/session_store.rs`
  - schema 2とschema 1移行、layout検証、JSON値をschema判別後に検証する読み込み経路を実装した。
- `src/app/tests.rs`
  - focused／visible、ポインターfocus、Ctrl+Tab表示順、ペイン移動アンカー、欠落ファイル復元、分割session復元などを追加した。
- `docs/tasks/README.md`、本指示書、本報告
  - `ui-improvement/`索引と実施記録を追加した。

新しいcrate依存は追加していない。

### 4.2 タブ同一性と文書状態

- `TabState.tabs`と`PrototypeApp.documents`は、開いた順の文書レジストリとして同じ位置対応を維持する。
- 表示順とペイン所属だけを`TabId`列として別管理し、reorder／moveではレジストリと`DocumentTab`を動かさない。
- `TabId`は単調発行し、close後も再利用しない。eguiのタブIDも`PaneId`と`TabId`から作る。
- したがって、worker、`document_id`、ページ、倍率、検索、選択、注釈cache、Undo履歴、dirty、保存・印刷、tile、thumbnail、errorは移動前と同じ`DocumentTab`へ残る。
- close時だけタブレジストリと文書レジストリの同じ位置を削除し、従来の非同期event走査との対応を維持する。

### 4.3 タブ操作とペイン状態

- タブバーは従来どおりメニューバーと共通ツールバーの間に1段で配置する。分割時だけ同じ段を分割比率で2グループへ区切り、各タブの所属ペインと選択・focusを示す。
- タブのタイトル領域は`click_and_drag`、close領域はclick専用とし、closeからD&Dを開始しない。
- ドラッグ中はタブバーの左端・間・右端へ挿入線を描画する。同一ペインでは選択を維持したままreorderする。
- 単一ペインのPDF領域は左右30%をsplit候補、中央40%をcancel領域とした。元ペインに1件以上残せないsplitは拒否する。
- 2ペイン時は反対側のタブバーへ指定位置でmoveでき、反対側PDF領域へのdropはそのペイン末尾へのmoveとする。3ペイン目は作らない。
- move／closeで空になったペインは即座に縮約する。残存ペインの選択、文書状態、分割比率を保持し、消滅したペインの`PageViewport`操作を破棄する。
- `Ctrl+Tab`は文書レジストリ順ではなく、focusedペイン内の現在の表示順だけを循環する。

### 4.4 フォーカス、入力、表示

- 各ペインは選択Tab IDを持ち、別にfocused Pane IDを1件持つ。`active_index()`はfocusedペインの選択文書だけを返す。
- 共通ツールバー、サイドバー、検索欄、保存、Undo、印刷、copy、ページショートカットはfocused文書を対象にする。
- PDF領域・タブのpressに加え、通常ホイールとpinch／Ctrl+ホイールでもポインター下のペインへfocusを移す。zoomはfocus更新後の文書へ適用する。
- 注釈editor、picker、popupなど`Middle`／`Foreground` layerがポインターを所有する位置では、背後のペインfocusとタブdropを行わない。
- `PageViewport`は`HashMap<PaneId, PageViewport>`とし、テキストdrag、空白pan、右クリック対象などをペイン間で共有しない。
- ScrollArea IDはペイン階層を含むため、タブを別ペインへ移す直前に文書の中央アンカーを復元アンカーへ移し、ページ位置を新しいIDへ引き継ぐ。
- FitWidth／FitPageには、上部タブバーと共通UIを除いた各ペインの実際の利用可能サイズを渡す。
- セパレーターは6 pt、各ペインは可能な限り160 pt以上とし、保存ratioの0.1..=0.9制約と現在の実幅の両方でclampする。

### 4.5 visible文書、非同期処理、cache

- `visible_indices()`は各ペインの選択Tab IDを文書レジストリ位置へ変換する。focusedの変更だけではvisible集合を変えない。
- Tile、Text snapshot、Annotation pageはvisibleであり、かつ既存generation／revision／wanted条件を満たす場合に受理する。非focused側も描画を継続する。
- ペイン内選択変更でvisibleから外れた文書だけ、render、text、annotation、search要求を無効化する。focus変更だけでは反対側の描画要求を破棄しない。
- focusを失ったペインの一時gestureとautoscrollは停止する。ネイティブfocus喪失、modal、ファイルpickerでは両visible文書と全ペインの一時操作を停止する。
- resident memory休止候補からvisible文書をすべて除外する。closeで新しくvisibleになった文書はfocusedかどうかにかかわらずresumeする。
- GPU LRUとthumbnail LRUのcache keyは従来どおり`document_id`を含み、表示位置変更の影響を受けない。

### 4.6 セッションschema 2

- schema 2はグローバルな`tabs`に加えて、1件または2件のpane、各paneのタブ番号列と選択番号、focused pane番号、split方向とratioを保存する。
- 方向は現在`Horizontal`だけを受理する。上下分割追加時に`left`／`right`固定fieldを移行する必要はない。
- 非空sessionでは全タブ番号がちょうど1ペインへ所属し、paneが空でなく、選択番号が所属し、focusが範囲内であることを検証する。2ペインは有限な0.1..=0.9の水平splitを必須とする。
- schema 1は専用private型として旧規則を検証後、全タブを1ペインへ所属させ、旧`selected_tab`をその選択へ変換する。
- 復元時に存在しない／開けないPDFは除外する。選択文書が除外された場合は後続、なければ直前を選び、空ペインを落としてfocusを有効なペインへ戻す。
- 非同期Open失敗でも通常closeと同じ縮約を通す。非同期`Opened`到着順では選択・focusを変更しない。

## 5．検証結果

### 5.1 自動検証

最終差分に対して次を実行した。

- `cargo fmt --all -- --check`: 成功。
- `git diff --check`: 成功。
- `docker compose -f .devcontainer/compose.base.yml exec workspace cargo check`: 成功。
- `docker compose -f .devcontainer/compose.base.yml exec workspace cargo test`: 245 passed、2 ignored、失敗0件。
- `docker compose -f .devcontainer/compose.base.yml exec workspace cargo check --release`: 成功。
- `docker compose -f .devcontainer/compose.base.yml exec workspace cargo clippy --all-targets -- -D warnings`: 成功。
- Windows GNU debug cross buildと`dist/lunapdf-debug.exe`への配置: 成功。
- Windows GNU release cross buildと`dist/lunapdf-release.exe`への配置: 成功。

ignored 2件は、外部viewer確認用PDFを生成するテストと、明示的な性能matrix生成テストであり、今回も実行していない。

### 5.2 追加・更新した確認

- Tab IDの単調性と非再利用、51件超のタブ、表示順を変えてもレジストリ順を変えないこと。
- 左右split、split拒否、ペイン間move、全挿入位置、空ペイン縮約、選択・focus規則、無効layoutの原子拒否。
- 左右矩形の非重複・全域被覆、最小幅clamp、極小windowの非負寸法、drop左右端とcancel中央。
- 非focused visible文書のtile／text／annotation受理、hidden文書の拒否、visible文書の休止除外。
- ポインター下ペインへのwheel／zoom focusとzoom対象、focusedペイン表示順による`Ctrl+Tab`。
- floating window／popupの背後入力遮断、ペイン移動時のcontinuous／single page中央アンカー引継ぎ。
- 単一の上部タブ段が左右両PDFペインより上にあり、分割時も同じ高さの2グループとして描画されること。
- schema 1移行、schema 2の単一／分割roundtrip、重複／欠落所属、不正選択、不正focus／ratio、51件超、欠落PDF、非同期Open失敗、分割所属・選択・focus・ratioのアプリ復元。

### 5.3 独立レビュー

`reviewer`サブエージェントで最終差分を読み取りレビューした。初回レビューで次を検出し、修正と回帰テストを追加した。

1. 非focusedペイン上のwheel／pinchがfocusとzoom対象を更新しない問題。
2. `Ctrl+Tab`がreorder後も文書レジストリ順を使う問題。
3. 注釈editor／pickerなど前面UI上の入力が背後ペインへ貫通する問題。

安定ID、visible非同期結果、hidden取消、schema移行・検証、失敗時縮約には追加のactionable issueは報告されなかった。
修正後の追補レビューでは、初回3件の解消とzoomの二重適用がないことを確認し、新しいactionable issueは報告されなかった。

### 5.4 GUI・実機確認

- `.codex/validation-lessons.md`を読んだ後、Computer Useで`dist/lunapdf-release.exe`を対象にWindows GUI起動を試みた。
- アプリ実行承認が時間切れとなり、対象windowを取得できなかった。したがって実GUIでのD&D、セパレーター操作、annotation、session再起動確認は未実施である。
- 100%、125%、150%、200%相当のDPI実機確認、マウス／touchpad別入力、日本語IME、長い日本語ファイル名の手動確認も未実施である。
- 多数タブは51件の自動テスト、分割比率は純粋関数とsession roundtrip、セッション欠落・失敗は自動テストで確認した。

## 6．初回実装時点の残る制約

- ペイン数は最大2、分割方向は左右のみである。3ペイン、nested split、別windowへの切り離しには対応しない。
- 同じ正規パスのPDFは既存タブを選ぶため、同一PDFの複数ビューには対応しない。
- 上下分割を追加する場合は、`SplitDirection`への値追加、高さ方向のworkspace矩形・drop領域・セパレーター、最小高さ、手動受入を追加する。Tab ID、pane所属、文書レジストリ、非同期対応を作り直す必要はない。
- タブdrag中の水平自動スクロールは実装していない。現在表示されているタブバー範囲内では左端・間・右端へdropでき、通常時の水平scrollは維持する。見えていない遠方へ移す場合は、先にタブバーをscrollする必要がある。
- 性能改善は実測していない。2ペインではvisible tile要求が2文書分になるが、GPU LRUの共有byte上限は従来どおりである。
- GUI承認が得られなかったため、手動受入項目とDPI別の視覚確認は残っている。自動テストとcross buildの成功をGUI実機成功としては扱わない。

## 7．共有タブ列・複数分割セット・上下分割への追補

### 7.1 変更理由

初回実装では、分割時にタブ列もペインごとのグループへ分かれていた。今回の追補では、タブの置き場所を変えずに Floorp／Firefox 型の「1本の共有タブ列と、結合された分割ペア」へ改めた。通常タブを選んだときに既存の分割を破棄せず、複数の分割セットをタブ列内へ保持できることも必要だったため、ペイン所有モデルを順序付きの単独／分割エントリモデルへ置き換えた。

### 7.2 状態と操作

- `TabState`はグローバルな`Single`／`Split`エントリ列を持つ。各タブは必ず1エントリだけに所属し、`Split`は安定ID、2タブ、左右／上下方向、比率、最後にフォーカスした側を保持する。
- タブバーは常に1本である。分割セット全体の外幅は通常タブ1件と同じにし、その共有枠内に2タイトル、個別の閉じるボタン、中央の境界線を描画する。中央の方向マークは表示せず、境界線の周囲をセット移動のドラッグ領域として使う。表示中の操作対象側だけを単独タブの選択時と同じ背景色と下線で示す。タイトル選択は対象セットと対象側を復元し、通常タブ選択ではそのタブだけを表示する。
- 分割メンバーを同じペアの相手へ落とすと配置を交換する。ペア外のタブ挿入位置へ落とすと2件の通常タブへ解除し、中央ハンドルのドラッグではセット全体を1エントリとして並べ替える。
- ドラッグ中はポインターへ半透明のタブ形状を追従させ、通常タブでは対象ファイル名、分割セット全体では2件のファイル名を表示する。
- PDFページがスクロールによって中央ペイン外へ延びても、PDFの押下、右クリック、ホバー、カーソル判定はページ矩形と実クリップ矩形の交差範囲だけで受け付ける。上部のタブ／ツールバー、下部のデバッグ／ステータス領域、左サイドバーの背後ではPDF操作を開始しない。
- 単独表示のPDF面では左・右・上・下の端へのドロップで方向対応の分割を作る。四隅は正規化した最寄りの辺を採用し、中央は変更しない。
- 分割表示のPDF面へタブを落とすと対象側だけを原子的に差し替える。通常タブとの交換では外れたタブをドラッグ元へ戻し、別セット間では両セットを維持してメンバーを交換する。
- 分割ペアの両タイトルと中央ハンドルの右クリックには、左右配置、上下配置、配置交換、分割解除を追加した。右クリックだけではアクティブセットを変更しない。
- 分割メンバーを閉じると相方を同じ位置の通常タブへ縮約する。新規PDFはアクティブエントリの直後へ通常タブとして開き、既存PDFの再オープンは所属セットを選択する。
- `Ctrl+Tab`は分割セットを1巡回単位として扱い、再表示時にはセットが保持するフォーカス側を復元する。

### 7.3 レイアウトとセッション

- 左右／上下を共通の矩形計算へ統合した。比率は`0.1..=0.9`、最小ペイン寸法は両軸とも160 pt、セパレーターは6 ptである。左右ではX座標と横リサイズカーソル、上下ではY座標と縦リサイズカーソルを使用する。
- Fit Width／Fit Pageには、分割方向にかかわらず各ペインの実矩形を渡す。
- セッションをschema 3へ更新し、順序付きの単独／分割エントリ、アクティブタブ、各セットの方向・比率・フォーカス側を保存する。
- schema 1は全タブを通常エントリへ移行する。schema 2は、単一ペインを通常エントリへ、2ペインの選択2件を1つの左右分割セットへ変換し、残りを旧タブバー順に通常エントリへ移行する。
- 復元できないPDFが分割の片側だけなら残った側を通常タブ化し、両側ならセットを除去する。不正な重複所属、方向、比率、フォーカスメンバーは読み込み時に拒否する。

### 7.4 変更範囲

- `src/domain/tabs.rs`: 共有順序の単独／分割エントリ、複数セット、抽出、交換、縮約、セット単位巡回。
- `src/domain/session.rs`、`src/persistence/session_store.rs`: schema 3、schema 1／2移行、復元検証。
- `src/app/workspace.rs`: 左右／上下の矩形、四辺ドロップ、方向対応ハイライト。
- `src/app.rs`: 共有タブバー、結合ペア、右クリック管理、上下セパレーター、PDF面D&D。
- `src/app/tests.rs`: 共有タブ段、複数セットを含む復元、フォーカス・可視状態の回帰確認。

新しいcrate依存、外部公開API、3画面以上の同時表示は追加していない。元の作業指示書は変更していない。

### 7.5 追補差分の検証

- `cargo fmt --all -- --check`: 成功。
- `git diff --check`: 成功。
- Dev Containerの`cargo check`、`cargo check --release`: 成功。
- Dev Containerの`cargo test`: 249 passed、2 ignored、失敗0件。
- Dev Containerの`cargo clippy --all-targets -- -D warnings`: 成功。
- Windows GNU debug／releaseのcross build: 成功。初回追補時の`dist`配置も成功した。その後の表示・入力追補では実行中のdebug／release実行ファイルがWindowsにロックされ、最新buildの`dist`上書きのみ未実施である。
- 分割セットと通常タブの外幅一致をUIテストで確認した。
- Computer Useでは`dist/lunapdf-release.exe`の起動、対象`LunaPDF`ウィンドウ、ネイティブの「開く」ダイアログまで確認した。対象ウィンドウのGraphics Captureが`0x80004002`、ダイアログ操作が`foreground window did not report a process id`で停止したため、左右／上下作成、複数セット切替、片側交換、ドラッグ解除、右クリック解除、再起動復元の実GUI操作は未判定である。自動検証の成功をGUI成功としては扱わない。

### 7.6 独立レビューの追補

最終差分を`reviewer`サブエージェントで読み取りレビューし、次の3件を修正した。

1. 同一セットの非フォーカス側を反対面へ落として配置交換したとき、activeタブとセットのfocused側が不一致になる問題。
2. 分割解除、メンバー抽出、別セットからの持ち出し、非表示セットの片側closeで、第2面から単独表示の第1面へ変わる文書にScrollAreaの中央アンカーが引き継がれない問題。
3. schema 3とランタイム復元が、activeな分割メンバーと保存されたfocusedメンバーの不一致を受理する検証漏れ。

各修正に回帰テストを追加し、その後にformat、全テスト、check、release check、Clippy、Windows GNU debug／release buildを再実行した。
