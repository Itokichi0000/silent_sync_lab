# ベンチマーク結果（Phase 5）

EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.

計測日: 2026-11-21。手順と指標の定義は [`BENCHMARK_PROTOCOL.md`](BENCHMARK_PROTOCOL.md)。
**この文書の表は `silent-sync export` の出力を機械的に貼ったもので、手で書いた数値は無い。**
「5. 読み取れること」に引用した数値は、同じ結果ファイルから計算したものである。

| 生データ | 内容 |
|---|---|
| [`benchmarks/all_runs.json`](benchmarks/all_runs.json) | 全 300 試行（形式 `silent-sync-lab/benchmark-v1`） |
| [`benchmarks/all_runs.csv`](benchmarks/all_runs.csv) | 同じ内容の CSV（1 行 1 試行） |
| [`benchmarks/all_runs.md`](benchmarks/all_runs.md) | `export --format markdown` の全文（全試行の表を含む） |
| [`benchmarks/bench_node.json`](benchmarks/bench_node.json), [`bench_node.log`](benchmarks/bench_node.log) | 実ノードのチェーン構成と、Oracle が先端の高さを報告するまでの時間（参考値） |

## 1. 何を測ったか

- 4 バックエンド × 5 モード × 3 範囲（10 / 100 / 1000 ブロック）× 各 5 回 = **300 試行。失敗 0、結果の食い違い 0**（各ファイルで、全 run の検出集合が最初の run と一致）。
- バックエンド: `fixture`（オフラインのフルブロック）、`local-index`（同じ fixture からクライアント内で計算した tweak インデックス）、
  `core`（Bitcoin Core 32.1 の REST、ループバック）、`oracle-v2`（BlindBit Oracle `f9ab7fd` の HTTP、ループバック。ブロックの同一性は同じノードのヘッダから取る）。
- `fixture` / `local-index` と `core` / `oracle-v2` は**別のチェーン**を測っている（前者は 1 ブロック 5 tx のプレースホルダ署名、後者は 1 ブロック 4 tx の実署名）。両者の数値を直接比べてはならない。
- 計測機: 4 論理 CPU（Intel Xeon 2.80GHz 相当の仮想マシン）、メモリ 16 GB、Linux 6.18、一時ストアは ext 系ファイルシステム上。クラウド上の共有環境であり、絶対値は参考程度。

## 2. 環境（結果ファイルに記録されたもの）

| item | value |
|---|---|
| tool version | 0.1.0 |
| scanner version | sync-core/0.1.0+bdk_sp@bb373e566fa2 |
| compiler | rustc 1.97.0 (2d8144b78 2026-07-07) |
| target | x86_64-unknown-linux-gnu |
| build profile | release |
| OS / architecture | linux / x86_64 |
| logical CPUs | 4 |
| note | offline deterministic chain, 10 blocks, 4 transactions per block, one payment to the receiver every 10 blocks |
| note | temporary stores on ext2/ext3 |
| note | offline deterministic chain, 100 blocks, 4 transactions per block, one payment to the receiver every 10 blocks |
| note | offline deterministic chain, 1000 blocks, 4 transactions per block, one payment to the receiver every 10 blocks |
| note | Bitcoin Core 32.1 (regtest, loopback REST); BlindBit Oracle commit f9ab7fd (HTTP, loopback) |
| note | real signed chain, 10 blocks, 3 transactions per block, one payment to the receiver every 10 blocks |
| note | real signed chain, 100 blocks, 3 transactions per block, one payment to the receiver every 10 blocks |
| note | real signed chain, 1000 blocks, 3 transactions per block, one payment to the receiver every 10 blocks |

## 3. 結果（成功した run の中央値。`silent-sync export --format markdown` の出力そのまま）

| range | blocks | backend | mode | runs | errors | wall median ms | min | max | spread % | CPU median ms | downloaded bytes | requests | tx seen | tx scanned | detected | same result |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1..=10 | 10 | fixture | scan | 5 | 0 | 4.08 | 3.99 | 5.34 | 33 | 0 | 0 | 0 | 50 | 21 | 1 | yes |
| 1..=10 | 10 | fixture | sync_cold | 5 | 0 | 21.14 | 18.11 | 27.73 | 45 | 10 | 0 | 0 | 50 | 21 | 1 | yes |
| 1..=10 | 10 | fixture | sync_resume | 5 | 0 | 4.98 | 4.95 | 5.45 | 10 | 0 | 0 | 0 | 25 | 11 | 1 | yes |
| 1..=10 | 10 | fixture | sync_noop | 5 | 0 | 0.96 | 0.91 | 1.10 | 19 | 0 | 0 | 0 | 0 | 0 | 1 | yes |
| 1..=10 | 10 | fixture | restore | 5 | 0 | 17.32 | 16.01 | 25.63 | 56 | 10 | 0 | 0 | 50 | 21 | 1 | yes |
| 1..=10 | 10 | local-index | scan | 5 | 0 | 4.06 | 3.91 | 5.83 | 47 | 0 | 0 | 0 | 21 | 21 | 1 | yes |
| 1..=10 | 10 | local-index | sync_cold | 5 | 0 | 17.81 | 17.00 | 23.35 | 36 | 10 | 0 | 0 | 21 | 21 | 1 | yes |
| 1..=10 | 10 | local-index | sync_resume | 5 | 0 | 5.68 | 4.94 | 5.98 | 18 | 0 | 0 | 0 | 11 | 11 | 1 | yes |
| 1..=10 | 10 | local-index | sync_noop | 5 | 0 | 0.95 | 0.91 | 1.08 | 18 | 0 | 0 | 0 | 0 | 0 | 1 | yes |
| 1..=10 | 10 | local-index | restore | 5 | 0 | 19.93 | 17.27 | 20.80 | 18 | 10 | 0 | 0 | 21 | 21 | 1 | yes |
| 117..=126 | 10 | core | scan | 5 | 0 | 15.61 | 15.27 | 17.58 | 15 | 10 | 99308 | 22 | 40 | 20 | 1 | yes |
| 117..=126 | 10 | core | sync_cold | 5 | 0 | 31.35 | 30.18 | 33.60 | 11 | 20 | 99861 | 24 | 40 | 20 | 1 | yes |
| 117..=126 | 10 | core | sync_resume | 5 | 0 | 14.38 | 13.92 | 14.68 | 5 | 10 | 95196 | 15 | 20 | 10 | 1 | yes |
| 117..=126 | 10 | core | sync_noop | 5 | 0 | 5.69 | 5.24 | 15.94 | 188 | 0 | 89913 | 3 | 0 | 0 | 1 | yes |
| 117..=126 | 10 | core | restore | 5 | 0 | 33.46 | 29.32 | 34.58 | 16 | 20 | 99861 | 24 | 40 | 20 | 1 | yes |
| 117..=126 | 10 | oracle-v2 | scan | 5 | 0 | 18.27 | 17.56 | 25.75 | 45 | 10 | 97111 | 12 | 20 | 20 | 1 | yes |
| 117..=126 | 10 | oracle-v2 | sync_cold | 5 | 0 | 34.95 | 32.65 | 41.93 | 27 | 10 | 97664 | 14 | 20 | 20 | 1 | yes |
| 117..=126 | 10 | oracle-v2 | sync_resume | 5 | 0 | 16.21 | 15.03 | 22.66 | 47 | 0 | 93934 | 10 | 10 | 10 | 1 | yes |
| 117..=126 | 10 | oracle-v2 | sync_noop | 5 | 0 | 6.16 | 5.63 | 6.24 | 10 | 0 | 89913 | 3 | 0 | 0 | 1 | yes |
| 117..=126 | 10 | oracle-v2 | restore | 5 | 0 | 32.78 | 32.71 | 40.55 | 24 | 10 | 97664 | 14 | 20 | 20 | 1 | yes |
| 1..=100 | 100 | fixture | scan | 5 | 0 | 39.87 | 39.19 | 61.24 | 55 | 40 | 0 | 0 | 500 | 205 | 10 | yes |
| 1..=100 | 100 | fixture | sync_cold | 5 | 0 | 57.48 | 54.05 | 88.48 | 60 | 50 | 0 | 0 | 500 | 205 | 10 | yes |
| 1..=100 | 100 | fixture | sync_resume | 5 | 0 | 26.10 | 25.82 | 26.88 | 4 | 20 | 0 | 0 | 250 | 102 | 10 | yes |
| 1..=100 | 100 | fixture | sync_noop | 5 | 0 | 6.16 | 5.75 | 6.49 | 12 | 10 | 0 | 0 | 0 | 0 | 10 | yes |
| 1..=100 | 100 | fixture | restore | 5 | 0 | 56.05 | 53.55 | 60.38 | 12 | 40 | 0 | 0 | 500 | 205 | 10 | yes |
| 1..=100 | 100 | local-index | scan | 5 | 0 | 39.23 | 38.76 | 44.30 | 14 | 40 | 0 | 0 | 205 | 205 | 10 | yes |
| 1..=100 | 100 | local-index | sync_cold | 5 | 0 | 55.00 | 53.46 | 56.58 | 6 | 40 | 0 | 0 | 205 | 205 | 10 | yes |
| 1..=100 | 100 | local-index | sync_resume | 5 | 0 | 26.30 | 25.64 | 28.64 | 11 | 20 | 0 | 0 | 102 | 102 | 10 | yes |
| 1..=100 | 100 | local-index | sync_noop | 5 | 0 | 5.78 | 5.77 | 6.21 | 8 | 0 | 0 | 0 | 0 | 0 | 10 | yes |
| 1..=100 | 100 | local-index | restore | 5 | 0 | 56.21 | 51.39 | 58.08 | 12 | 40 | 0 | 0 | 205 | 205 | 10 | yes |
| 117..=216 | 100 | core | scan | 5 | 0 | 115.99 | 103.08 | 128.94 | 22 | 90 | 184539 | 202 | 400 | 200 | 10 | yes |
| 117..=216 | 100 | core | sync_cold | 5 | 0 | 138.95 | 132.33 | 149.05 | 12 | 90 | 185092 | 204 | 400 | 200 | 10 | yes |
| 117..=216 | 100 | core | sync_resume | 5 | 0 | 67.42 | 63.12 | 76.23 | 19 | 50 | 137818 | 105 | 200 | 100 | 10 | yes |
| 117..=216 | 100 | core | sync_noop | 5 | 0 | 5.84 | 5.56 | 6.49 | 16 | 0 | 89913 | 3 | 0 | 0 | 10 | yes |
| 117..=216 | 100 | core | restore | 5 | 0 | 130.62 | 118.67 | 151.27 | 25 | 100 | 185092 | 204 | 400 | 200 | 10 | yes |
| 117..=216 | 100 | oracle-v2 | scan | 5 | 0 | 139.68 | 136.62 | 157.50 | 15 | 50 | 165421 | 102 | 200 | 200 | 10 | yes |
| 117..=216 | 100 | oracle-v2 | sync_cold | 5 | 0 | 156.11 | 141.34 | 160.58 | 12 | 60 | 165974 | 104 | 200 | 200 | 10 | yes |
| 117..=216 | 100 | oracle-v2 | sync_resume | 5 | 0 | 81.18 | 73.25 | 89.00 | 19 | 40 | 128089 | 55 | 100 | 100 | 10 | yes |
| 117..=216 | 100 | oracle-v2 | sync_noop | 5 | 0 | 6.23 | 5.78 | 7.03 | 20 | 0 | 89913 | 3 | 0 | 0 | 10 | yes |
| 117..=216 | 100 | oracle-v2 | restore | 5 | 0 | 168.85 | 161.99 | 171.62 | 6 | 60 | 165974 | 104 | 200 | 200 | 10 | yes |
| 1..=1000 | 1000 | fixture | scan | 5 | 0 | 408.81 | 402.50 | 423.16 | 5 | 410 | 0 | 0 | 5000 | 2050 | 100 | yes |
| 1..=1000 | 1000 | fixture | sync_cold | 5 | 0 | 475.67 | 458.73 | 589.37 | 27 | 430 | 0 | 0 | 5000 | 2050 | 100 | yes |
| 1..=1000 | 1000 | fixture | sync_resume | 5 | 0 | 261.56 | 253.15 | 263.75 | 4 | 240 | 0 | 0 | 2500 | 1025 | 100 | yes |
| 1..=1000 | 1000 | fixture | sync_noop | 5 | 0 | 56.96 | 55.16 | 58.85 | 6 | 60 | 0 | 0 | 0 | 0 | 100 | yes |
| 1..=1000 | 1000 | fixture | restore | 5 | 0 | 469.36 | 449.26 | 548.36 | 21 | 420 | 0 | 0 | 5000 | 2050 | 100 | yes |
| 1..=1000 | 1000 | local-index | scan | 5 | 0 | 387.93 | 384.22 | 413.04 | 7 | 390 | 0 | 0 | 2050 | 2050 | 100 | yes |
| 1..=1000 | 1000 | local-index | sync_cold | 5 | 0 | 465.24 | 453.42 | 486.53 | 7 | 410 | 0 | 0 | 2050 | 2050 | 100 | yes |
| 1..=1000 | 1000 | local-index | sync_resume | 5 | 0 | 258.80 | 256.81 | 269.84 | 5 | 240 | 0 | 0 | 1025 | 1025 | 100 | yes |
| 1..=1000 | 1000 | local-index | sync_noop | 5 | 0 | 57.86 | 55.56 | 60.47 | 8 | 50 | 0 | 0 | 0 | 0 | 100 | yes |
| 1..=1000 | 1000 | local-index | restore | 5 | 0 | 468.12 | 460.36 | 514.81 | 12 | 420 | 0 | 0 | 2050 | 2050 | 100 | yes |
| 117..=1116 | 1000 | core | scan | 5 | 0 | 1217.42 | 1197.85 | 1258.63 | 5 | 950 | 1041803 | 2020 | 4000 | 2000 | 100 | yes |
| 117..=1116 | 1000 | core | sync_cold | 5 | 0 | 1208.02 | 1167.65 | 1315.55 | 12 | 930 | 1042356 | 2022 | 4000 | 2000 | 100 | yes |
| 117..=1116 | 1000 | core | sync_resume | 5 | 0 | 636.14 | 551.87 | 665.28 | 18 | 480 | 566176 | 1013 | 2000 | 1000 | 100 | yes |
| 117..=1116 | 1000 | core | sync_noop | 5 | 0 | 6.62 | 6.07 | 6.99 | 14 | 10 | 89913 | 3 | 0 | 0 | 100 | yes |
| 117..=1116 | 1000 | core | restore | 5 | 0 | 1277.41 | 1212.34 | 1328.02 | 9 | 970 | 1042356 | 2022 | 4000 | 2000 | 100 | yes |
| 117..=1116 | 1000 | oracle-v2 | scan | 5 | 0 | 1296.80 | 1213.99 | 1558.91 | 27 | 550 | 850672 | 1020 | 2000 | 2000 | 100 | yes |
| 117..=1116 | 1000 | oracle-v2 | sync_cold | 5 | 0 | 1366.11 | 1305.21 | 1575.70 | 20 | 570 | 851225 | 1022 | 2000 | 2000 | 100 | yes |
| 117..=1116 | 1000 | oracle-v2 | sync_resume | 5 | 0 | 656.23 | 639.67 | 797.67 | 24 | 280 | 470660 | 513 | 1000 | 1000 | 100 | yes |
| 117..=1116 | 1000 | oracle-v2 | sync_noop | 5 | 0 | 7.29 | 6.58 | 7.62 | 14 | 0 | 89913 | 3 | 0 | 0 | 100 | yes |
| 117..=1116 | 1000 | oracle-v2 | restore | 5 | 0 | 1394.60 | 1298.69 | 1668.22 | 26 | 570 | 851225 | 1022 | 2000 | 2000 | 100 | yes |

## 4. 計測範囲

| range | network | first block hash | last block hash |
|---|---|---|---|
| 1..=10 | regtest | `5e58aabdd19a91a8da013ece8c410bbcc25e319315e0d9f1dc1a1cefccf55dd8` | `0bf26a595c4d0b8e47cb5feeb3628c1862f64969c79b4867b56ca259f8478753` |
| 1..=100 | regtest | `5e58aabdd19a91a8da013ece8c410bbcc25e319315e0d9f1dc1a1cefccf55dd8` | `2b5ad4ea8d08bb0fea7205070361d74d90159c63a8563f88a8140e859f1035f8` |
| 1..=1000 | regtest | `5e58aabdd19a91a8da013ece8c410bbcc25e319315e0d9f1dc1a1cefccf55dd8` | `686f05e2082f38999369e989559dc9234a7174b84a01d6d4e2afb1e23e74d736` |
| 117..=126 | regtest | `7a18be69c7a1e0715af754fd8b71137b199028366cd75e36bac625c2fc163609` | `44f66fc8c64bd957b8d792bfb49e61856e823f03fb15f3780c39b67f97b78e35` |
| 117..=216 | regtest | `7a18be69c7a1e0715af754fd8b71137b199028366cd75e36bac625c2fc163609` | `11c17e529855c1c1d88af0ea544cbe84b1547660e6d389c0c5065b9902baed0f` |
| 117..=1116 | regtest | `7a18be69c7a1e0715af754fd8b71137b199028366cd75e36bac625c2fc163609` | `59287285130bffc9e4d0927c23a31e95361710f850688615e1a1d13ac583fa9b` |

## 5. 読み取れること

数値は `benchmarks/all_runs.json` から計算した中央値。

### 5.1 規模に対する伸び（T06）

| バックエンド | 10 ブロック | 100 ブロック | 1000 ブロック |
|---|---|---|---|
| `fixture` scan（ms / ブロック） | 0.408 | 0.399 | 0.409 |
| `local-index` scan（ms / ブロック） | 0.406 | 0.392 | 0.388 |
| `core` scan（ms / ブロック） | 1.561 | 1.160 | 1.217 |
| `oracle-v2` scan（ms / ブロック） | 1.827 | 1.397 | 1.297 |

- 時間はブロック数にほぼ比例する。10 ブロックで 1 ブロックあたりが大きいのは、接続とヘッダ検証の固定費が効くため。
- `core` / `oracle-v2` は接続のたびに **約 90 KB（89,913 バイト）** を必ず取得する。genesis から先端（高さ 1116）までのヘッダ鎖を検証するためで、`sync_noop`（することが無い同期）でもこの量になる。
  これは「ノードが言うチェーン名を信じず、genesis からの連結を確かめる」（DECISIONS D-029）ことの対価で、チェーンが長いほど増える（1 ヘッダ 80 バイト）。
  実際の signet（数十万ブロック）では数十 MB になる計算で、**ヘッダ鎖のキャッシュを永続化しない現在の実装は、長いチェーンでは接続のたびにこの費用を払う**（未対応。O-016）。

### 5.2 Core 直結と Oracle 経由（同じチェーン、同じ範囲）

| 1000 ブロックの scan | `core` | `oracle-v2` | 比（oracle / core） |
|---|---|---|---|
| wall（ms） | 1217.42 | 1296.80 | 1.07 |
| クライアントの CPU（ms） | 950 | 550 | 0.58 |
| 受信バイト数 | 1,041,803 | 850,672 | 0.82 |
| リクエスト数 | 2020 | 1020 | — |
| ブロックあたりの受信（固定費を除く） | 約 952 バイト | 約 761 バイト | — |

- **この条件では、Oracle 経由は速くならなかった**（wall は 100 ブロックで 1.20 倍、1000 ブロックで 1.07 倍）。
  どちらも 1 ブロックにつき HTTP リクエストを 1〜2 回送っており、ブロックが極端に小さい（1 ブロック 1 KB 未満）ため、時間の大半はリクエストの往復で、スキャンの計算ではない。
- クライアントの CPU 時間は Oracle 経由の方が少ない（0.58 倍）。入力の検査と tweak の計算（公開鍵の抽出・加算）をサーバが済ませているため。
  **その分の計算はサーバ側に移っただけで、消えたわけではない。** サーバ側の費用は測れていない（参考: Oracle の `/info` が先端の高さ 1116 を報告するまで、起動を含めて約 0.75 秒。これがインデックス完了までの時間を正しく表すかは確認していない: UNVERIFIED）。
- 受信バイト数の差は小さい（0.82 倍）。regtest のブロックには受信者に関係のない tx がほとんど無く、フルブロックでも小さいため。
  mainnet のように大半の tx が無関係なチェーンでは差が大きく開くはずだが、**それはこの計測からは言えない**（UNVERIFIED）。
- Oracle の HTTP JSON は 16 進文字列で、1 項目あたりの表現はバイナリのブロックより冗長である。gRPC（バイナリ）では異なる可能性がある（未計測。O-014）。

### 5.3 永続化の費用と再開

- `sync_cold` は `scan` に対し、10 ブロックで約 16〜17 ms 余分にかかる（新しい SQLite ファイルの作成とスキーマ、`synchronous = FULL` のコミット）。1000 ブロックでは差は誤差に埋もれる（`core` で 0.99 倍、`fixture` で 1.16 倍）。
  コミットは 100 ブロックごとに 1 回。コミット 1 回あたりの費用は、この計測では分離していない。
- `sync_resume`（後半だけ）は `sync_cold` の 0.48〜0.56 倍（100・1000 ブロック）。スキャン済みの範囲を取り直していない（取り直さないこと自体は、同期スイートの `syncing_again_changes_nothing` が検査している）。
- `sync_noop` は範囲に依らず一定（`core` で約 6 ms、3 リクエスト）。`fixture` で範囲に比例して増えるのは、毎回 fixture ファイル全体を読み込んで検査しているため（1000 ブロックで接続に約 56 ms）で、同期処理の費用ではない。
- `restore`（ストア削除 → 同じ birthday から再構築）は `sync_cold` と同じ時間（0.94〜1.08 倍、100・1000 ブロック）で、**削除前のストアと検出結果が完全に一致**した（全 60 回）。

### 5.4 `local-index` について

`local-index` は「フルブロックを取得し、クライアント内でインデックスを計算してからスキャンする」もので、数値は `fixture` とほぼ同じになる（インデックスの計算もクライアントの CPU 時間に含まれる）。
インデックスサーバを使った場合のクライアント費用の見積もりには**ならない**。意味があるのは「同じデータから 2 通りの経路で同じ結果が出る」ことの確認である。

### 5.5 ばらつき

- 60 組（バックエンド × モード × 範囲）の (最大 − 最小) / 中央値は、中央値で約 15%。最大は `core` / `sync_noop` / 10 ブロックの 188%（5 回のうち 1 回だけ 15.94 ms、他は 5〜6 ms）。
- 10 ブロックのような短い run は、数 ms の揺れがそのまま数十 % になる。CPU 時間は分解能 10 ms のため、短い run では 0 か 10 になる。

## 6. この結果から言えないこと

1. **インターネット越しの性能**。すべてループバック。往復遅延が加われば、リクエスト数（Core 直結はブロックあたり 2、Oracle 経由は 1）の差が効くはずだが、測っていない。
2. **mainnet 規模**。ブロックの大きさ・tx 数が桁違いに小さい。1 ブロックあたりの時間・バイト数を mainnet に外挿してはならない。
3. **インデックス方式の総コスト**。サーバ側の前処理・保存・運用の費用は含まれていない。クライアントの CPU が減った分は、サーバが払っている。
4. **数十万ブロックの同期**。最大 1000 ブロック。ヘッダ鎖の検証費用（5.1）は長いチェーンで支配的になり得る。
5. **サーバのキャッシュが冷えた状態**。ノードの再起動や OS のページキャッシュ破棄はしていない。
6. **他の機械での絶対値**。共有の仮想マシン 1 台、1 日の結果。
7. **正しさの証明**。全 run の結果が一致したことは、経路どうしが整合していることを示すだけで、結果が完全であることの証明ではない（`FAULT_INJECTION.md`）。
   なお実ノードのチェーンでは、送信側が作った 100 出力（`bench_node.json` の `expected_detections`）と検出数 100 が一致している。

## 7. 再現

`BENCHMARK_PROTOCOL.md` §5 のコマンド。このレポートは次で生成した。

```bash
target/release/silent-sync export --input offline_10.json --input offline_100.json --input offline_1000.json \
    --input node_10.json --input node_100.json --input node_1000.json --format markdown
```
