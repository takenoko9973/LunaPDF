# 開発環境の運用手順

Dev ContainerでLinux検証やWindows GNU cross-buildを行うときに使う手順です。どの検査が必要かは[検証マトリクス](tasks/development-environment/verification-matrix.md)で選びます。[2026-10-21の実測報告](tasks/development-environment/261021_lunapdf_test_cache_optimization_report.md)は当時の測定値と経緯を記録したもので、現在の実行手順はこの文書を参照してください。

## 対象checkoutとCompose projectの照合

文書だけの変更など、Dev Containerを使わない作業ではDockerを確認しません。containerで検査を実行するときは、PowerShellで対象リポジトリのrootから始めます。

    $checkoutRoot = (Resolve-Path .).Path
    $gitRoot = git rev-parse --show-toplevel
    if ((Resolve-Path $gitRoot).Path -ne $checkoutRoot) { throw '対象checkout rootが一致しません。' }
    docker compose ls

docker compose ls の Name、Status、Config Files から候補を選び、Config Files が対象checkoutの設定を指していることを確認します。project名をフォルダー名から推測せず、既存projectの全Composeファイルを表示順のまま使います。VS Code Dev Containersが生成したComposeファイルが一覧に含まれる場合も、それらを省略しません。

    $composeProject = '<docker compose ls の Name>'
    $composeFiles = @(
        '<Config Files の1件目>',
        '<Config Files の2件目>'
    )
    $composeArgs = @('-p', $composeProject)
    foreach ($file in $composeFiles) {
        $composeArgs += @('-f', $file)
    }

同じ引数でComposeの解決結果とworkspace serviceを調べます。

    docker compose @composeArgs config
    docker compose @composeArgs ps --all workspace
    $workspaceIds = docker compose @composeArgs ps --quiet workspace
    foreach ($id in $workspaceIds) {
        docker inspect --format '{{json .Mounts}}' $id
    }

対象checkoutの絶対パスと、実際に稼働するworkspace containerの /workspace bind mount source が同じ作業ディレクトリを指すことを確認します。Docker DesktopではWindows側の `D:\checkout\LunaPDF` がcontainer host側で `/run/desktop/mnt/host/d/checkout/LunaPDF` のように見えることがあるため、その変換を踏まえて照合します。Compose configの値だけでは、既存containerが実際に使うmountを確認したことにはなりません。対象serviceがrunningであることも ps の結果で確認します。

候補projectが曖昧、workspace containerがない、または実mountが対象checkoutと一致しない場合は、そのcontainerに対してexecしません。対象checkoutの設定からDev Containers環境を起動する必要がある場合は、タスク上Linux検証が必要なときだけ起動します。起動後にCompose一覧とmountをもう一度確認します。project名やcontainer名を推測して別projectへ接続しません。

## 逐次検証

照合済みのworkspaceがrunningなら、そのprojectへのexecを使い、既存のCargo targetとincremental cacheを再利用します。変更に関連する検査だけを[検証マトリクス](tasks/development-environment/verification-matrix.md)から選びます。

    docker compose @composeArgs exec workspace cargo check
    docker compose @composeArgs exec workspace cargo clippy --all-targets

workspaceが停止中またはまだ作られておらず、対象projectとmount設定を確認済みで、既存imageを使えば足りる場合に限り、必要なときに起動します。

    docker compose @composeArgs up -d --no-deps --no-build --pull never workspace

一つの検査だけが必要で常駐workspaceを起動する必要がない場合は、一時containerを使えます。Compose configでmount sourceが対象checkoutを指し、`services.workspace.image` で解決したimageがローカルにあることを次のコマンドで確かめてから one-off `run` を実行します。imageが存在しない場合や判別できない場合は `run` を実行せず止めます。

    docker image inspect '<configで確認したworkspace image>'

実行対象のCargo検査はマトリクスに従って指定します。

    docker compose @composeArgs run --rm --no-deps --pull never workspace cargo check --locked

Compose runには --no-build optionがありません。`--pull never` はpullを防ぎますが、build禁止にはなりません。サービスに `build` と `image` の両方が設定され、imageが見つからない場合にbuildされることがあるため、上のローカルimage確認を省略しません。`--build` は明示的なbuildを行う指定なので、通常のone-off実行には付けません。upには `--no-build` と `--pull never` の両方を指定でき、意図しないbuildやpullを避けられます。詳細は[Composeのbuildとimageの仕様](https://docs.docker.com/reference/compose-file/build/#using-build-and-image)と[`docker compose run`のoption](https://docs.docker.com/reference/cli/docker/compose/run/#options)を参照してください。

## Windows GNU cross-build

Windows GNU向けbuildを受け入れ条件が求める場合に限り、照合済みworkspaceで既存の手順を使います。debugとreleaseの成果物は分け、Windows側のsource配置を移動しません。

    docker compose @composeArgs exec workspace sh -c "cargo build --target=x86_64-pc-windows-gnu && install -D target/x86_64-pc-windows-gnu/debug/lunapdf.exe /workspace/dist/lunapdf-debug.exe"

    docker compose @composeArgs exec workspace sh -c "cargo build --release --target=x86_64-pc-windows-gnu && install -D target/x86_64-pc-windows-gnu/release/lunapdf.exe /workspace/dist/lunapdf-release.exe"

これらはGNU cross-buildのコマンド例であり、通常のRust変更に毎回要求する検査ではありません。Windows MSVCと実機GUIの検証範囲は、検証マトリクスに従い、自動検査と実機未検証を分けて記録します。

## cacheの境界と独立checkoutの並列検証

現在のCompose構成では、workspace image tagは共通です。Cargo registry volumeも明示名で共有します。一方、Cargo target volumeはCompose projectごとに分かれるため、通常の逐次開発では同じprojectのworkspaceを使ってincremental buildを再利用します。

| 資産 | 現在の境界 | 使い方 |
| --- | --- | --- |
| workspace image | 共通tag lunapdf_devcontainer-workspace:latest | build入力が変わっていなければタスクごとに作り直さない |
| Cargo registry | Compose projectをまたいで共有 | 既存volumeを再利用し、複製・削除しない |
| Cargo target | Compose projectごと | 逐次作業では同じprojectを使い、別project間では共有しない |
| sccache | cache-parallel overlayで使う共有volume | 独立checkoutの並列compileに限り、適用条件を確認して使う |

独立した別checkoutを同時にcompileするときだけ、対象checkoutのbase Composeとcache-parallel overlayを同じprojectに重ね、checkoutごとに明示的に異なるCompose project名を使います。overlayはRUSTC_WRAPPER=/usr/bin/sccache、SCCACHE_DIR=/var/cache/sccache、CARGO_INCREMENTAL=0を設定します。overlayを使う場合でもcheckoutを作成したり、同じsource treeを並列編集したりしません。

    $parallelCheckoutRoot = (Resolve-Path '<独立checkout root>').Path
    $parallelProject = '<この独立checkout用に明示した一意のproject名>'
    $parallelComposeArgs = @(
        '-p', $parallelProject,
        '-f', (Join-Path $parallelCheckoutRoot '.devcontainer/compose.base.yml'),
        '-f', (Join-Path $parallelCheckoutRoot '.devcontainer/overlays/cache-parallel.compose.yml')
    )
    docker compose @parallelComposeArgs config

Compose configのmount sourceが `$parallelCheckoutRoot` と一致することを確認します。また、解決した `services.workspace.image` がローカルに存在することを確認してから、そのimageに `/usr/bin/sccache` があるか one-off containerで調べます。

    docker image inspect '<parallelComposeArgs configで確認したworkspace image>'
    docker compose @parallelComposeArgs run --rm --no-deps --pull never workspace sh -c "test -x /usr/bin/sccache"

この確認が失敗し、今回の並列compileに必要なら、対象checkout/projectで明示的にimageをbuildしてから再確認します。

    docker compose @parallelComposeArgs build workspace

build後、resolved imageが存在し、必要な実行ファイルが含まれることを再確認します。確認できなければ並列compileを実行しません。

    docker image inspect '<parallelComposeArgs configで確認したworkspace image>'
    docker compose @parallelComposeArgs run --rm --no-deps --pull never workspace sh -c "test -x /usr/bin/sccache"

実際に検証するtestやbuildは、変更に関係する最小範囲を[検証マトリクス](tasks/development-environment/verification-matrix.md)で選びます。既存testを使う場合の対象例は次のとおりです。

    docker compose @parallelComposeArgs run --rm --no-deps --pull never workspace cargo test --bin lunapdf single_page_view_line_wheel_navigates_pages_and_respects_boundaries

sccacheの再利用には、container内のsource絶対パス、compiler、flags、target、profileなどの入力条件が一致する必要があります。通常の逐次buildではCARGO_INCREMENTALを無効化せず、既存projectへoverlayを付け外ししません。共有cacheのhit率や容量は測定値で確認し、効果を別のbuild条件へ外挿しません。

## Dockerfile変更、container更新、切り戻し

Dockerfile、build context、選択中overlayのimage build入力を変え、タスクがそのimageを必要とする場合にだけ、対象checkout/projectを再照合したうえで明示的にbuildします。

    docker compose @composeArgs build workspace
    docker compose @composeArgs up -d --no-deps --force-recreate --no-build --pull never workspace

imageのbuildだけでは稼働中containerは更新されません。VS Code Dev Containersを使う場合は対象checkoutの Dev Containers: Rebuild Container を実行し、完了後にprojectとmountを再確認します。他者のcontainerや別checkoutを再作成しません。通常のsource変更やdocs-only作業ではimageをbuildしません。

設定変更を戻す必要があれば、対象のDockerfile/Compose変更を戻し、必要な場合だけ自分のprojectのimageとworkspaceを更新します。既存image、container、volume、Cargo target、registry、sccacheを無断で削除・pruneしません。docker compose down -vや全体的なcache purgeは使いません。不明な資産の所有者や用途が判別できない場合は片付けず、確認が必要と報告します。
