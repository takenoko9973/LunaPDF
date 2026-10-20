# LunaPDF Codex指示・検証・開発環境の最適化報告

2026-10-21。作業指示書が求めた、LunaPDF内の指示導線、検証判断、Dev Container運用、過剰なDocker許可の是正をまとめる。

## 変更結果

- [AGENTS.md](../../../AGENTS.md) は、LunaPDFの不変条件と作業時に必要な文書への条件付き導線を残し、固定Compose／Windows GNU buildコマンドを[開発環境の運用手順](../../development-workflow.md)へ移した。指示書を新規作成する場合だけ分類・索引へ登録する方針にし、既存文書の移動・改名を作業開始条件にしない。
- [検証マトリクス](verification-matrix.md) は変更に応じた最小検証の選択を担当する。[開発環境の運用手順](../../development-workflow.md) はcheckout・Compose project・mountの照合、逐次検証、Windows GNU cross-build、cache境界、image更新と切り戻しのコマンドを担当する。[検証事例](../../../.codex/validation-lessons.md) はGUI実機受入または性能受入が明示された場合に参照する履歴資料として導入部だけを更新し、過去本文を保持した。
- 元の最適化指示書を [アーカイブ](261021_lunapdf_codex_instructions_docs_skills_optimization.md) に原文保存し、分類索引へ登録した。保存先は28,770 bytes、SHA-256 `cc8af08103ed936b75d75feb0c1811f6fb4db879b6a4ad359b200abc0f768be7`。元文書と保存先のhash一致を確認した。
- 旧Docker/Cargo報告には、現行の運用手順への案内を1箇所だけ追加した。既存の測定値・手順・歴史的記述は書き換えていない。
- `.codex/rules/docker.rules` のrepo-local prefix allowを削除した。rule確認時のCLI `codex-cli 0.163.0-alpha.10.2` で、ファイルを単独指定した `codex execpolicy check --pretty --rules .codex/rules/docker.rules -- docker compose down -v` と `... -- docker compose exec workspace sh -c 'echo sample'` はどちらも `allow` と判定された。これはrule単独のmatch評価であり、稼働中Codexの全layerを合成した実効policyやDocker実操作を示すものではない。

## 文書の責務と参照範囲

| 扱い | 内容と配置 |
| --- | --- |
| `AGENTS.md`で常時適用 | commit/version方針、Windows sourceの所在、ユーザー文書・clipboard・Docker資産の保護、結果の正直な報告、およびLunaPDFの不変条件 |
| 仕様・振る舞いを変える場合だけ | 変更根拠は[`docs/current-spec.md`](../../current-spec.md)で確認 |
| Dev Container/build環境を操作する場合だけ | checkout・Compose project・mountの照合と具体的なCompose/build/cacheコマンドは[`docs/development-workflow.md`](../../development-workflow.md) |
| 検証範囲を選ぶとき | 変更に対する最小の検証選択は[`verification-matrix.md`](verification-matrix.md) |
| GUI実機受入または性能受入が明示された場合だけ | 過去のGUI・性能検証事例と制約は[`.codex/validation-lessons.md`](../../../.codex/validation-lessons.md) |
| 実施結果 | 日付付き作業報告は実行時点の結果・測定値を履歴として保持 |

既存のsource・tests・Cargo/CI設定・`.devcontainer`実設定・toolkit sourceに今回変更すべき根拠はなく、変更していない。既存の`docs/current-spec.md`にも根拠のない新しい保証は追加していない。

現在の指示導線では、挙動・仕様を変える場合だけ `docs/current-spec.md`、Dev Container/build環境を操作する場合だけ `docs/development-workflow.md`、GUI実機受入または性能受入が明示された場合だけ `.codex/validation-lessons.md` を参照する。新しいSkillは0件とした。今回のmulti-step workflowは安定したrunbookで扱え、同じ誤操作が繰り返されたなどSkillを追加する根拠は確認されなかった。

AGENTS.mdのサイズは、基準commit `815521406e84b225b06876f5f72bb306da6ef66e` のblobをUTF-8として計測した3,974 bytes / 2,360 UTF-16 code unitsから、現行3,510 bytes / 1,862 code unitsとなった。計測はNode.jsの `Buffer.byteLength` 相当とUTF-8 decode後の文字列 `.length` による。日本語等はBMP内の文字で、code point数も同数だった。token数と作業時間の前後比較は未測定であり、bytesや文字数から推定していない。

## Dev Containerの安全境界

現行の稼働workspaceについては、Compose `ls` / `config` / `ps` と対象containerのread-only `inspect` で、実際の `/workspace` bind sourceが現在のcheckoutを指すことをWindowsからDocker Desktopへのパス表記変換を踏まえて確認した。報告・手順にはユーザー名を含む絶対パスを記録せず、一般的なDocker Desktop mount表記だけを示す。

one-off `run` には `--no-build` optionがない。`--pull never` もbuild禁止を意味しない。Composeの `build` と `image` の組合せはimageがない場合にbuildへ進むことがあるため、手順ではresolved `services.workspace.image` の `docker image inspect` を成功させ、imageがないときは `run` しないようにした。並列cacheの例では、cache-parallel overlayを使う独立checkoutに限定し、対象imageに `/usr/bin/sccache` が存在することをone-off checkで確かめ、必要な場合だけ明示buildする。build後も同じimageと実行ファイルの確認を繰り返し、確認できなければ並列compileを止める。またfull suiteを無条件に使わず、検証マトリクスで関連範囲を選び、実在する既存testを例示した。[Compose Build Specification](https://docs.docker.com/reference/compose-file/build/#using-build-and-image) と [`docker compose run` options](https://docs.docker.com/reference/cli/docker/compose/run/#options) を参照。

ruleの削除はrepo-local fileだけに適用される。Codexのruleはtrusted project layerで扱われ、rule設定は再起動時に再読込されるため、既存sessionのloaded state更新にはCodex再起動が必要な場合がある（[Codex rule documentation](https://learn.chatgpt.com/docs/agent-configuration/rules)）。user/globalまたは他layerのruleは変更しておらず、全Docker操作が禁止されたとは主張しない。危険なDocker commandは実行していない。

## 検証と適用範囲

- 文書の変更なのでRust test、build、GUI確認のregression testは追加していない。コード挙動を変えず、test artifactを追加してもこの変更の振る舞いを有意に検証できない。
- 作業ツリーのtracked差分に対する `git diff --check` はexit 0だった。ステージ後の全体 `git diff --cached --check` は、指示書アーカイブ原文にある2行のMarkdown hardbreakを指摘してexit 1となったため、原文保持を優先してそこは変更していない。workflowと本reportのEOF空行は除去した。アーカイブを除外した `git diff --cached --check` はrootが再stage後に実行し、exit 0だった。
- validation-lessonsの歴史本文は、見出し `# LunaPDF検証メモ（Codex向け）` からEOFまで7,608 bytes、SHA-256 `0179ce10027909ca7bff4e884d15bc73eb38fb220cd3d4c951d702f56c008667` で変更前と一致した。
- local relative linksと、workflowに例示した既存test名を確認した。対象testは `src/app/tests.rs` に存在するが、本Taskでは実行していない。
- TaskBから受け取った最終証拠では、`docs/current-spec.md`と`docs/tasks/README.md`のrelative link 67件は全解決し、未解決0件、Markdown anchor付き0件、untrackedのcurrent-specを含む末尾空白0行だった。参照test 29件（重複を含む31参照）は各source file内の宣言と照合し、function/method 14種と`SearchState`・`DisplayMode`・`ZoomMode`・`ViewState`も宣言を確認した。TaskBでもRust test/build/GUIは未実行。
- `docker compose run --help`、`up --help`、`build --help` 等のread-only helpで、`run` の `--build` / `--pull` / `--no-deps` / `--rm`、`up` の `--no-build` / `--pull` / `--force-recreate`、`build` のservice引数を確認した。Dockerのbuild/start/stop/delete、cache削除、GUI操作は行っていない。
- 削除後に `.codex/rules/docker.rules` が不在であることと、`.codex/rules/` の差分が当該file削除だけであることを確認した。全layerの実効policyを合成した評価は行っていない。

Codex toolkitのread-only監査はrootから受け取った基準commit `a87328670e4b10b3a659e496d6cfd3b098878a7a` の証拠に基づく。`AGENTS.sync.md:51,63`、`README.md:12,16,42`、`install-codex-skills.ps1:1246` の既存責務から、Task/writer/Git境界、任意review、local trust、Skill sync範囲が確認され、共通source、installer、user configの変更は不要と判断された。role TOMLやskillのfile sizeを常時投入contextの合計とは扱わず、actual token/timeは未測定とした。Toolkit sourceへ変更・再installはしていない。
