# ベンチマーク手順（Phase 5）

EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.

何を、どの条件で、どう測るかを定める。結果は `docs/BENCHMARK_REPORT.md`、生データは `docs/benchmarks/`。
**数値を手で書き写さない。** レポートの表は `silent-sync export` の出力をそのまま貼る。

## 1. 測るもの・測らないもの

| 指標 | 意味 | 取得方法 |
|---|---|---|
| `wall_ms` | 計測区間の経過時間（ソースを開くところから、結果が確定するまで） | `std::time::Instant` |
| `connect_ms` | そのうち、ソースを開くのにかかった時間 | 同上 |
| `cpu_ms` | **クライアントプロセスの** CPU 時間（user + system） | Linux の `/proc/self/stat`。分解能は 1 tick（通常 10 ms）。他の OS では `null` |
| `requests` / `downloaded_bytes` | 計測区間に送った HTTP リクエスト数と、受信した本文のバイト数 | `ScanSource::stats()` の差分。Oracle バックエンドは Oracle とヘッダ用ノードの合計 |
| `txs_seen` | 見た tx の数（候補数） | スキャンのカウンタ |
| `txs_scanned` | BIP352 の適格条件を満たし実際にスキャンした tx の数 | 同上 |
| `detected_count` | 範囲全体で検出した出力数 | 同上 |
| `verification_status` | そのファイルの最初の成功 run と検出集合が一致するか | 自動 |
| `error` | 失敗した run のエラー（失敗も 1 件として記録する） | 自動 |

**測らないもの（`null` のまま。推定値を入れない）**

- `server_preprocess_ms`: サーバ側の前処理時間。ノードのブロック検証・保存、Oracle の tweak 計算は、どのバックエンドからも取得できない。
  参考値として、Oracle の `/info` が先端の高さを報告するまでの経過時間を `bench_node` が別に出力する（起動時間を含む粗い値で、インデックス完了を正しく表すかは未確認。レポートに注記として載せる）。
- サーバ側の CPU・メモリ・ストレージ。
- ネットワーク遅延。すべて同じ機械のループバックで動かす。**インターネット越しの性能は何も示さない。**

## 2. モード

| モード | 準備（計測外） | 計測区間 | キャッシュ条件 |
|---|---|---|---|
| `scan` | なし | ソースを開く → 範囲全体をステートレスにスキャン | クライアント cold。サーバのキャッシュは制御しない |
| `sync_cold` | なし | ソースを開く → 新しいストアを作る → 範囲全体を同期 | 同上 |
| `sync_resume` | 新しいストアを範囲の中央まで同期 | ソースを開く → ストアを開く → 残り半分を同期 | 準備でサーバは温まっている |
| `sync_noop` | 新しいストアを範囲の終端まで同期 | ソースを開く → ストアを開く → 同期（することが無い） | warm |
| `restore` | 新しいストアを終端まで同期 → 検出結果を控える → ストアを削除 | ソースを開く → 同じ birthday で新しいストアを作る → 同期 → 控えと一致するか確認 | 準備でサーバは温まっている |

- ストアは SQLite（`synchronous = FULL`）。チャンクごとに 1 回コミットし、既定のチャンクは 100 ブロック。ストアを置く場所（`--work-dir`）のファイルシステムで fsync のコストが変わるので、レポートに明記する。
- 毎回プロセス内で新しいソース接続を作る。プロセスの起動時間は含まない。

## 3. 同条件の定義（設計書 §10.1）

- 同じ機械、同じビルド、同じチェーン（開始・終了ブロックのハッシュを結果に記録）、同じ鍵（公開テスト鍵）、同じラベル集合（`--label 1`）。
- バックエンド × モードを 1 巡ずつ、`--repeat` 回繰り返す（ラウンドロビン）。時間経過による変動が特定のバックエンドに偏らないようにするため。実行順は結果に記録する。
- 各 (バックエンド, モード, 範囲) について 5 回以上。**中央値・最小・最大・ばらつき（(最大 − 最小) / 中央値）**を出す。全試行を残す。
- リリースビルド（`--release`）で測る。デバッグビルドの結果はレポートに使わない（結果ファイルに `build_profile` が残る）。
- 計測中は他の重い処理（テスト、ビルド）を動かさない。動いていた場合はやり直す。

## 4. 測定対象

| ID | バックエンド | データ | 範囲 |
|---|---|---|---|
| A | `fixture`（フルブロック、オフライン） | 生成した決定的チェーン | 10 / 100 / 1000 ブロック |
| B | `local-index`（fixture から計算した tweak インデックス） | 同上 | 同上 |
| C | `core`（Bitcoin Core REST、フルブロック） | 実ノード上の署名済み tx のチェーン | 10 / 100 / 1000 ブロック |
| D | `oracle-v2`（BlindBit Oracle HTTP + 自ノードのヘッダ） | 同じ実ノード | 同上 |

- A / B のチェーン: 1 ブロックあたり coinbase + 4 tx（他人宛ての支払い、Taproot 同士、Taproot 出力なし、非適格入力）。10 ブロックごとに受信者宛ての支払い 1 件。署名はプレースホルダ（コンセンサス上は無効）。
- C / D のチェーン: 1 ブロックあたり coinbase + 3 tx（silent payment、Taproot 同士、Taproot 出力なし）。10 ブロックごとに受信者宛て。すべて実際に署名されたコンセンサス有効な tx。
  計測範囲はシナリオの最初のブロックから始まる（それ以前の資金準備ブロックは範囲外）。
- **A/B と C/D はチェーンが違うので、数値を直接比べない。** 比べてよいのは A と B、C と D、および同じバックエンドの範囲違い・モード違い。
- 実ノードのブロックは非常に小さい（数百バイト〜1 KB 台）。**mainnet のブロックの大きさ・tx 数とは桁が違う**ので、絶対値は実運用の目安にならない。

## 5. 再現コマンド

```bash
# 0. バイナリ（リポジトリの外に置く）
export BITCOIND_EXE="$(VERIFY_SIGNATURES=1 scripts/fetch_bitcoind.sh /path/outside/repo/core)"
export BLINDBIT_ORACLE_EXE="$(scripts/build_blindbit_oracle.sh /path/outside/repo/oracle)"
cargo build --release --locked -p lab-cli -p lab-testkit
OUT=/path/outside/repo/bench && mkdir -p "$OUT"

# 1. オフラインのチェーン（A, B）
for n in 10 100 1000; do
  target/release/gen_fixture --long $n "$OUT/long_$n.json"
  target/release/silent-sync bench --backends fixture,local-index --fixture "$OUT/long_$n.json" \
      --label 1 --repeat 5 --work-dir "$OUT" --output "$OUT/offline_$n.json" \
      --note "offline fixture chain, $n blocks"
done

# 2. 実ノードと Oracle（C, D）。bench_node は URL と範囲を JSON で 1 行出力し、stdin が閉じるまで動き続ける
mkfifo "$OUT/hold" && (sleep infinity > "$OUT/hold" &)
target/release/bench_node 1000 10 < "$OUT/hold" > "$OUT/node.json" &
#   node.json から rest_url / oracle_url / from を読む。範囲は from..from+9, from..from+99, from..from+999
target/release/silent-sync bench --backends core,oracle-v2 \
    --core-rest-url "$REST" --oracle-url "$ORACLE" --from $FROM --to $((FROM + N - 1)) \
    --label 1 --repeat 5 --work-dir "$OUT" --output "$OUT/node_$N.json" \
    --note "Bitcoin Core <version>; BlindBit Oracle <commit>; regtest, $N blocks"

# 3. レポート
target/release/silent-sync export --input "$OUT"/offline_10.json ... --format markdown
target/release/silent-sync export --input ... --format csv  --output docs/benchmarks/all_runs.csv
target/release/silent-sync export --input ... --format json --output docs/benchmarks/all_runs.json
```

`bench` は失敗した run や結果の食い違いがあると終了コード 1 を返す（結果ファイルは書かれ、失敗も記録される）。既存の結果ファイルは上書きしない。

## 6. 結果ファイル

- 形式 `silent-sync-lab/benchmark-v1`（JSON）。`environment`（ツール・スキャナ・コンパイラのバージョン、ターゲット、ビルド種別、OS、論理 CPU 数、`--note`）と `runs`（全試行）。
- **個人データを含めない**: txid、出力、ウォレット識別子、ファイルパス、URL は入らない（テスト `results_hold_no_personal_data`）。計測範囲の最初と最後のブロックハッシュは入る。
- `export` は複数の結果ファイルをまとめられる（同じビルド・同じ機械のものに限る。違えば拒否）。

## 7. 既知の限界

1. クライアント側だけの計測である。インデックス方式は「サーバが先に計算しておく」方式なので、クライアントの数値だけを比べると前処理の分だけ有利に見える。総コストの比較にはならない。
2. ループバック、小さなブロック、短い範囲（最大 1000）。インターネット、mainnet 規模、数十万ブロックについては何も言えない。
3. サーバのキャッシュ状態は制御していない（ノードの再起動や OS のページキャッシュ破棄はしない）。「cold」はクライアント側の意味。
4. CPU 時間の分解能は 10 ms 程度。短い run では 0 か 10 になる。
5. 単一の機械・単一の日時の結果。機械が変われば絶対値は変わる。
6. 受信者宛ての支払いの密度（10 ブロックに 1 件）や、ブロックあたりの tx 数は任意に決めた値で、実際の利用を代表しない。
