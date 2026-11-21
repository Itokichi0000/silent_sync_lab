# ベンチマーク結果（Phase 5）

EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.

計測日: 2026-11-21。手順と指標の定義は [`BENCHMARK_PROTOCOL.md`](BENCHMARK_PROTOCOL.md)。

**§2〜§5 の表は `silent-sync export --format markdown` の出力を機械的に貼ったもので、手で書いた数値は無い**
（テスト `derived_figures_follow_from_the_medians` が派生値の計算を検査している）。
§1 の計測機の説明と §6 の文章は手で書いた。§6 で引用する数値は、すべて §3・§4 の表にあるものである。

| 生データ | 内容 |
|---|---|
| [`benchmarks/all_runs.json`](benchmarks/all_runs.json) | 全 300 試行（形式 `silent-sync-lab/benchmark-v1`） |
| [`benchmarks/all_runs.csv`](benchmarks/all_runs.csv) | 同じ内容の CSV（1 行 1 試行） |
| [`benchmarks/all_runs.md`](benchmarks/all_runs.md) | `export --format markdown` の全文（全試行の表を含む） |
| [`benchmarks/bench_node.json`](benchmarks/bench_node.json), [`bench_node.log`](benchmarks/bench_node.log) | 実ノードのチェーン構成と、Oracle が先端の高さを報告するまでの時間（参考値）。`bench_node` の出力から接続先の URL を除いたもの |

## 1. 何を測ったか

- 4 バックエンド × 5 モード × 3 範囲（10 / 100 / 1000 ブロック）× 各 5 回 = **300 試行。失敗 0、結果の食い違い 0**（各結果ファイルで、全 run の検出集合が最初の run と一致）。
- バックエンド: `fixture`（オフラインのフルブロック）、`local-index`（同じ fixture からクライアント内で計算した tweak インデックス）、
  `core`（Bitcoin Core 32.1 の REST、ループバック）、`oracle-v2`（BlindBit Oracle `f9ab7fd` の HTTP、ループバック。ブロックの同一性は同じノードのヘッダから取る）。
- `fixture` / `local-index` と `core` / `oracle-v2` は**別のチェーン**を測っている（前者は coinbase + 4 tx のプレースホルダ署名、後者は coinbase + 3 tx の実署名）。両者の数値を直接比べてはならない。
- 計測機（手で記録）: 4 論理 CPU の仮想マシン（`/proc/cpuinfo` は Intel Xeon 2.80GHz）、メモリ 16 GB、Linux 6.18、一時ストアは ext4。クラウド上の共有環境であり、絶対値は参考程度。
- 計測中、他の重い処理は動かしていない（計測前後の load average は 1.4〜1.6）。

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
| note | offline deterministic chain, 10 blocks, coinbase + 4 transactions per block, one payment to the receiver every 10 blocks |
| note | temporary stores on ext4 |
| note | offline deterministic chain, 100 blocks, coinbase + 4 transactions per block, one payment to the receiver every 10 blocks |
| note | offline deterministic chain, 1000 blocks, coinbase + 4 transactions per block, one payment to the receiver every 10 blocks |
| note | Bitcoin Core 32.1 (regtest, loopback REST); BlindBit Oracle commit f9ab7fd (HTTP, loopback) |
| note | real signed chain, 10 blocks, coinbase + 3 transactions per block, one payment to the receiver every 10 blocks |
| note | real signed chain, 100 blocks, coinbase + 3 transactions per block, one payment to the receiver every 10 blocks |
| note | real signed chain, 1000 blocks, coinbase + 3 transactions per block, one payment to the receiver every 10 blocks |

## 3. 結果（成功した run の中央値）

| range | blocks | backend | mode | runs | errors | wall median ms | min | max | spread % | CPU median ms | downloaded bytes | requests | tx seen | tx scanned | detected | same result |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1..=10 | 10 | fixture | scan | 5 | 0 | 3.99 | 3.97 | 4.09 | 3 | 0 | 0 | 0 | 50 | 21 | 1 | yes |
| 1..=10 | 10 | fixture | sync_cold | 5 | 0 | 22.03 | 19.45 | 24.56 | 23 | 10 | 0 | 0 | 50 | 21 | 1 | yes |
| 1..=10 | 10 | fixture | sync_resume | 5 | 0 | 5.12 | 5.00 | 7.61 | 51 | 0 | 0 | 0 | 25 | 11 | 1 | yes |
| 1..=10 | 10 | fixture | sync_noop | 5 | 0 | 1.17 | 0.95 | 1.47 | 45 | 0 | 0 | 0 | 0 | 0 | 1 | yes |
| 1..=10 | 10 | fixture | restore | 5 | 0 | 23.15 | 20.86 | 39.16 | 79 | 10 | 0 | 0 | 50 | 21 | 1 | yes |
| 1..=10 | 10 | local-index | scan | 5 | 0 | 4.39 | 4.05 | 6.17 | 48 | 10 | 0 | 0 | 21 | 21 | 1 | yes |
| 1..=10 | 10 | local-index | sync_cold | 5 | 0 | 22.47 | 19.67 | 27.62 | 35 | 10 | 0 | 0 | 21 | 21 | 1 | yes |
| 1..=10 | 10 | local-index | sync_resume | 5 | 0 | 5.77 | 4.57 | 6.96 | 41 | 10 | 0 | 0 | 11 | 11 | 1 | yes |
| 1..=10 | 10 | local-index | sync_noop | 5 | 0 | 0.97 | 0.91 | 1.58 | 69 | 10 | 0 | 0 | 0 | 0 | 1 | yes |
| 1..=10 | 10 | local-index | restore | 5 | 0 | 21.14 | 17.51 | 22.39 | 23 | 10 | 0 | 0 | 21 | 21 | 1 | yes |
| 117..=126 | 10 | core | scan | 5 | 0 | 17.41 | 15.11 | 19.89 | 27 | 10 | 99863 | 24 | 40 | 20 | 1 | yes |
| 117..=126 | 10 | core | sync_cold | 5 | 0 | 35.06 | 28.64 | 38.38 | 28 | 10 | 100416 | 26 | 40 | 20 | 1 | yes |
| 117..=126 | 10 | core | sync_resume | 5 | 0 | 15.36 | 12.97 | 16.29 | 22 | 20 | 95749 | 17 | 20 | 10 | 1 | yes |
| 117..=126 | 10 | core | sync_noop | 5 | 0 | 5.97 | 4.84 | 6.18 | 22 | 0 | 90466 | 5 | 0 | 0 | 1 | yes |
| 117..=126 | 10 | core | restore | 5 | 0 | 35.45 | 30.75 | 39.92 | 26 | 20 | 100416 | 26 | 40 | 20 | 1 | yes |
| 117..=126 | 10 | oracle-v2 | scan | 5 | 0 | 20.23 | 17.97 | 23.75 | 29 | 10 | 97825 | 15 | 20 | 20 | 1 | yes |
| 117..=126 | 10 | oracle-v2 | sync_cold | 5 | 0 | 40.56 | 34.53 | 44.79 | 25 | 10 | 98378 | 17 | 20 | 20 | 1 | yes |
| 117..=126 | 10 | oracle-v2 | sync_resume | 5 | 0 | 17.65 | 15.78 | 19.74 | 22 | 10 | 94648 | 13 | 10 | 10 | 1 | yes |
| 117..=126 | 10 | oracle-v2 | sync_noop | 5 | 0 | 6.10 | 5.60 | 10.32 | 77 | 10 | 90627 | 6 | 0 | 0 | 1 | yes |
| 117..=126 | 10 | oracle-v2 | restore | 5 | 0 | 35.35 | 32.06 | 37.19 | 15 | 10 | 98378 | 17 | 20 | 20 | 1 | yes |
| 1..=100 | 100 | fixture | scan | 5 | 0 | 40.10 | 38.64 | 47.97 | 23 | 40 | 0 | 0 | 500 | 205 | 10 | yes |
| 1..=100 | 100 | fixture | sync_cold | 5 | 0 | 61.92 | 55.27 | 65.65 | 17 | 40 | 0 | 0 | 500 | 205 | 10 | yes |
| 1..=100 | 100 | fixture | sync_resume | 5 | 0 | 26.35 | 26.10 | 27.41 | 5 | 30 | 0 | 0 | 250 | 102 | 10 | yes |
| 1..=100 | 100 | fixture | sync_noop | 5 | 0 | 6.56 | 5.95 | 9.69 | 57 | 10 | 0 | 0 | 0 | 0 | 10 | yes |
| 1..=100 | 100 | fixture | restore | 5 | 0 | 60.81 | 58.59 | 66.90 | 14 | 40 | 0 | 0 | 500 | 205 | 10 | yes |
| 1..=100 | 100 | local-index | scan | 5 | 0 | 42.37 | 38.39 | 54.38 | 38 | 40 | 0 | 0 | 205 | 205 | 10 | yes |
| 1..=100 | 100 | local-index | sync_cold | 5 | 0 | 61.03 | 54.45 | 67.92 | 22 | 40 | 0 | 0 | 205 | 205 | 10 | yes |
| 1..=100 | 100 | local-index | sync_resume | 5 | 0 | 26.81 | 25.75 | 27.59 | 7 | 20 | 0 | 0 | 102 | 102 | 10 | yes |
| 1..=100 | 100 | local-index | sync_noop | 5 | 0 | 6.79 | 5.94 | 12.38 | 95 | 10 | 0 | 0 | 0 | 0 | 10 | yes |
| 1..=100 | 100 | local-index | restore | 5 | 0 | 59.25 | 57.74 | 70.73 | 22 | 50 | 0 | 0 | 205 | 205 | 10 | yes |
| 117..=216 | 100 | core | scan | 5 | 0 | 132.32 | 120.01 | 147.37 | 21 | 100 | 185088 | 204 | 400 | 200 | 10 | yes |
| 117..=216 | 100 | core | sync_cold | 5 | 0 | 166.79 | 144.97 | 173.20 | 17 | 110 | 185641 | 206 | 400 | 200 | 10 | yes |
| 117..=216 | 100 | core | sync_resume | 5 | 0 | 90.77 | 77.17 | 98.79 | 24 | 70 | 138364 | 107 | 200 | 100 | 10 | yes |
| 117..=216 | 100 | core | sync_noop | 5 | 0 | 6.36 | 6.14 | 7.71 | 25 | 10 | 90466 | 5 | 0 | 0 | 10 | yes |
| 117..=216 | 100 | core | restore | 5 | 0 | 160.96 | 139.99 | 170.34 | 19 | 110 | 185641 | 206 | 400 | 200 | 10 | yes |
| 117..=216 | 100 | oracle-v2 | scan | 5 | 0 | 157.97 | 151.14 | 180.80 | 19 | 60 | 166135 | 105 | 200 | 200 | 10 | yes |
| 117..=216 | 100 | oracle-v2 | sync_cold | 5 | 0 | 176.61 | 172.07 | 182.69 | 6 | 60 | 166688 | 107 | 200 | 200 | 10 | yes |
| 117..=216 | 100 | oracle-v2 | sync_resume | 5 | 0 | 90.30 | 76.60 | 96.75 | 22 | 40 | 128803 | 58 | 100 | 100 | 10 | yes |
| 117..=216 | 100 | oracle-v2 | sync_noop | 5 | 0 | 6.76 | 6.28 | 9.05 | 41 | 0 | 90627 | 6 | 0 | 0 | 10 | yes |
| 117..=216 | 100 | oracle-v2 | restore | 5 | 0 | 176.35 | 161.49 | 185.69 | 14 | 70 | 166688 | 107 | 200 | 200 | 10 | yes |
| 1..=1000 | 1000 | fixture | scan | 5 | 0 | 402.28 | 399.29 | 432.18 | 8 | 400 | 0 | 0 | 5000 | 2050 | 100 | yes |
| 1..=1000 | 1000 | fixture | sync_cold | 5 | 0 | 496.04 | 451.65 | 506.16 | 11 | 440 | 0 | 0 | 5000 | 2050 | 100 | yes |
| 1..=1000 | 1000 | fixture | sync_resume | 5 | 0 | 261.97 | 256.31 | 269.38 | 5 | 240 | 0 | 0 | 2500 | 1025 | 100 | yes |
| 1..=1000 | 1000 | fixture | sync_noop | 5 | 0 | 58.42 | 57.61 | 69.80 | 21 | 60 | 0 | 0 | 0 | 0 | 100 | yes |
| 1..=1000 | 1000 | fixture | restore | 5 | 0 | 469.93 | 458.22 | 506.34 | 10 | 420 | 0 | 0 | 5000 | 2050 | 100 | yes |
| 1..=1000 | 1000 | local-index | scan | 5 | 0 | 394.88 | 389.38 | 409.60 | 5 | 390 | 0 | 0 | 2050 | 2050 | 100 | yes |
| 1..=1000 | 1000 | local-index | sync_cold | 5 | 0 | 456.38 | 452.29 | 521.57 | 15 | 410 | 0 | 0 | 2050 | 2050 | 100 | yes |
| 1..=1000 | 1000 | local-index | sync_resume | 5 | 0 | 253.94 | 251.61 | 280.99 | 12 | 240 | 0 | 0 | 1025 | 1025 | 100 | yes |
| 1..=1000 | 1000 | local-index | sync_noop | 5 | 0 | 58.88 | 54.51 | 60.82 | 11 | 60 | 0 | 0 | 0 | 0 | 100 | yes |
| 1..=1000 | 1000 | local-index | restore | 5 | 0 | 474.21 | 459.23 | 510.36 | 11 | 420 | 0 | 0 | 2050 | 2050 | 100 | yes |
| 117..=1116 | 1000 | core | scan | 5 | 0 | 1211.43 | 1177.38 | 1397.49 | 18 | 950 | 1042331 | 2022 | 4000 | 2000 | 100 | yes |
| 117..=1116 | 1000 | core | sync_cold | 5 | 0 | 1383.81 | 1270.46 | 1479.65 | 15 | 1010 | 1042884 | 2024 | 4000 | 2000 | 100 | yes |
| 117..=1116 | 1000 | core | sync_resume | 5 | 0 | 660.70 | 628.50 | 790.88 | 25 | 490 | 566724 | 1015 | 2000 | 1000 | 100 | yes |
| 117..=1116 | 1000 | core | sync_noop | 5 | 0 | 8.54 | 7.13 | 9.58 | 29 | 10 | 90466 | 5 | 0 | 0 | 100 | yes |
| 117..=1116 | 1000 | core | restore | 5 | 0 | 1427.83 | 1278.52 | 1704.25 | 30 | 1030 | 1042884 | 2024 | 4000 | 2000 | 100 | yes |
| 117..=1116 | 1000 | oracle-v2 | scan | 5 | 0 | 1620.24 | 1497.83 | 1642.33 | 9 | 610 | 851386 | 1023 | 2000 | 2000 | 100 | yes |
| 117..=1116 | 1000 | oracle-v2 | sync_cold | 5 | 0 | 1653.38 | 1440.84 | 1738.54 | 18 | 610 | 851939 | 1025 | 2000 | 2000 | 100 | yes |
| 117..=1116 | 1000 | oracle-v2 | sync_resume | 5 | 0 | 861.55 | 736.55 | 925.43 | 22 | 310 | 471374 | 516 | 1000 | 1000 | 100 | yes |
| 117..=1116 | 1000 | oracle-v2 | sync_noop | 5 | 0 | 7.84 | 7.15 | 9.09 | 25 | 10 | 90627 | 6 | 0 | 0 | 100 | yes |
| 117..=1116 | 1000 | oracle-v2 | restore | 5 | 0 | 1660.28 | 1511.25 | 1704.01 | 12 | 610 | 851939 | 1025 | 2000 | 2000 | 100 | yes |

## 4. 派生値（上の中央値から `export` が計算したもの）

### 4.1 範囲 1 ブロックあたり

| range | blocks | backend | mode | wall ms / block | CPU ms / block | bytes / block | requests / block |
|---|---|---|---|---|---|---|---|
| 1..=10 | 10 | fixture | scan | 0.399 | 0.000 | 0.0 | 0.00 |
| 1..=10 | 10 | fixture | sync_cold | 2.203 | 1.000 | 0.0 | 0.00 |
| 1..=10 | 10 | local-index | scan | 0.439 | 1.000 | 0.0 | 0.00 |
| 1..=10 | 10 | local-index | sync_cold | 2.247 | 1.000 | 0.0 | 0.00 |
| 117..=126 | 10 | core | scan | 1.741 | 1.000 | 9986.3 | 2.40 |
| 117..=126 | 10 | core | sync_cold | 3.506 | 1.000 | 10041.6 | 2.60 |
| 117..=126 | 10 | oracle-v2 | scan | 2.023 | 1.000 | 9782.5 | 1.50 |
| 117..=126 | 10 | oracle-v2 | sync_cold | 4.056 | 1.000 | 9837.8 | 1.70 |
| 1..=100 | 100 | fixture | scan | 0.401 | 0.400 | 0.0 | 0.00 |
| 1..=100 | 100 | fixture | sync_cold | 0.619 | 0.400 | 0.0 | 0.00 |
| 1..=100 | 100 | local-index | scan | 0.424 | 0.400 | 0.0 | 0.00 |
| 1..=100 | 100 | local-index | sync_cold | 0.610 | 0.400 | 0.0 | 0.00 |
| 117..=216 | 100 | core | scan | 1.323 | 1.000 | 1850.9 | 2.04 |
| 117..=216 | 100 | core | sync_cold | 1.668 | 1.100 | 1856.4 | 2.06 |
| 117..=216 | 100 | oracle-v2 | scan | 1.580 | 0.600 | 1661.3 | 1.05 |
| 117..=216 | 100 | oracle-v2 | sync_cold | 1.766 | 0.600 | 1666.9 | 1.07 |
| 1..=1000 | 1000 | fixture | scan | 0.402 | 0.400 | 0.0 | 0.00 |
| 1..=1000 | 1000 | fixture | sync_cold | 0.496 | 0.440 | 0.0 | 0.00 |
| 1..=1000 | 1000 | local-index | scan | 0.395 | 0.390 | 0.0 | 0.00 |
| 1..=1000 | 1000 | local-index | sync_cold | 0.456 | 0.410 | 0.0 | 0.00 |
| 117..=1116 | 1000 | core | scan | 1.211 | 0.950 | 1042.3 | 2.02 |
| 117..=1116 | 1000 | core | sync_cold | 1.384 | 1.010 | 1042.9 | 2.02 |
| 117..=1116 | 1000 | oracle-v2 | scan | 1.620 | 0.610 | 851.4 | 1.02 |
| 117..=1116 | 1000 | oracle-v2 | sync_cold | 1.653 | 0.610 | 851.9 | 1.02 |

### 4.2 同じ範囲でのバックエンド比（その範囲で最初に挙げたバックエンドに対する比）

| range | mode | backend | relative to | wall | CPU | bytes | requests |
|---|---|---|---|---|---|---|---|
| 1..=10 | scan | local-index | fixture | 1.10 | — | — | — |
| 1..=10 | sync_cold | local-index | fixture | 1.02 | 1.00 | — | — |
| 1..=10 | sync_resume | local-index | fixture | 1.13 | — | — | — |
| 1..=10 | sync_noop | local-index | fixture | 0.83 | — | — | — |
| 1..=10 | restore | local-index | fixture | 0.91 | 1.00 | — | — |
| 117..=126 | scan | oracle-v2 | core | 1.16 | 1.00 | 0.98 | 0.62 |
| 117..=126 | sync_cold | oracle-v2 | core | 1.16 | 1.00 | 0.98 | 0.65 |
| 117..=126 | sync_resume | oracle-v2 | core | 1.15 | 0.50 | 0.99 | 0.76 |
| 117..=126 | sync_noop | oracle-v2 | core | 1.02 | — | 1.00 | 1.20 |
| 117..=126 | restore | oracle-v2 | core | 1.00 | 0.50 | 0.98 | 0.65 |
| 1..=100 | scan | local-index | fixture | 1.06 | 1.00 | — | — |
| 1..=100 | sync_cold | local-index | fixture | 0.99 | 1.00 | — | — |
| 1..=100 | sync_resume | local-index | fixture | 1.02 | 0.67 | — | — |
| 1..=100 | sync_noop | local-index | fixture | 1.04 | 1.00 | — | — |
| 1..=100 | restore | local-index | fixture | 0.97 | 1.25 | — | — |
| 117..=216 | scan | oracle-v2 | core | 1.19 | 0.60 | 0.90 | 0.51 |
| 117..=216 | sync_cold | oracle-v2 | core | 1.06 | 0.55 | 0.90 | 0.52 |
| 117..=216 | sync_resume | oracle-v2 | core | 0.99 | 0.57 | 0.93 | 0.54 |
| 117..=216 | sync_noop | oracle-v2 | core | 1.06 | 0.00 | 1.00 | 1.20 |
| 117..=216 | restore | oracle-v2 | core | 1.10 | 0.64 | 0.90 | 0.52 |
| 1..=1000 | scan | local-index | fixture | 0.98 | 0.97 | — | — |
| 1..=1000 | sync_cold | local-index | fixture | 0.92 | 0.93 | — | — |
| 1..=1000 | sync_resume | local-index | fixture | 0.97 | 1.00 | — | — |
| 1..=1000 | sync_noop | local-index | fixture | 1.01 | 1.00 | — | — |
| 1..=1000 | restore | local-index | fixture | 1.01 | 1.00 | — | — |
| 117..=1116 | scan | oracle-v2 | core | 1.34 | 0.64 | 0.82 | 0.51 |
| 117..=1116 | sync_cold | oracle-v2 | core | 1.19 | 0.60 | 0.82 | 0.51 |
| 117..=1116 | sync_resume | oracle-v2 | core | 1.30 | 0.63 | 0.83 | 0.51 |
| 117..=1116 | sync_noop | oracle-v2 | core | 0.92 | 1.00 | 1.00 | 1.20 |
| 117..=1116 | restore | oracle-v2 | core | 1.16 | 0.59 | 0.82 | 0.51 |

### 4.3 同じバックエンドでのモード比

| range | backend | sync_cold / scan | sync_cold − scan (ms) | sync_resume / sync_cold | restore / sync_cold | sync_noop wall ms | sync_noop bytes |
|---|---|---|---|---|---|---|---|
| 1..=10 | fixture | 5.52 | 18.04 | 0.23 | 1.05 | 1.17 | 0 |
| 1..=10 | local-index | 5.11 | 18.08 | 0.26 | 0.94 | 0.97 | 0 |
| 117..=126 | core | 2.01 | 17.65 | 0.44 | 1.01 | 5.97 | 90466 |
| 117..=126 | oracle-v2 | 2.00 | 20.33 | 0.44 | 0.87 | 6.10 | 90627 |
| 1..=100 | fixture | 1.54 | 21.82 | 0.43 | 0.98 | 6.56 | 0 |
| 1..=100 | local-index | 1.44 | 18.66 | 0.44 | 0.97 | 6.79 | 0 |
| 117..=216 | core | 1.26 | 34.47 | 0.54 | 0.97 | 6.36 | 90466 |
| 117..=216 | oracle-v2 | 1.12 | 18.64 | 0.51 | 1.00 | 6.76 | 90627 |
| 1..=1000 | fixture | 1.23 | 93.76 | 0.53 | 0.95 | 58.42 | 0 |
| 1..=1000 | local-index | 1.16 | 61.51 | 0.56 | 1.04 | 58.88 | 0 |
| 117..=1116 | core | 1.14 | 172.38 | 0.48 | 1.03 | 8.54 | 90466 |
| 117..=1116 | oracle-v2 | 1.02 | 33.14 | 0.52 | 1.00 | 7.84 | 90627 |

## 5. 計測範囲

| range | network | first block hash | last block hash |
|---|---|---|---|
| 1..=10 | regtest | `5e58aabdd19a91a8da013ece8c410bbcc25e319315e0d9f1dc1a1cefccf55dd8` | `0bf26a595c4d0b8e47cb5feeb3628c1862f64969c79b4867b56ca259f8478753` |
| 1..=100 | regtest | `5e58aabdd19a91a8da013ece8c410bbcc25e319315e0d9f1dc1a1cefccf55dd8` | `2b5ad4ea8d08bb0fea7205070361d74d90159c63a8563f88a8140e859f1035f8` |
| 1..=1000 | regtest | `5e58aabdd19a91a8da013ece8c410bbcc25e319315e0d9f1dc1a1cefccf55dd8` | `686f05e2082f38999369e989559dc9234a7174b84a01d6d4e2afb1e23e74d736` |
| 117..=126 | regtest | `617a4a7bb93b55a3959719866b098934dc8c121d22c3a53ee6e07b2023c353f1` | `6374d740bd3f865251f633ec009fdac741b4a68f565718d9bd4ca5dc5f0c59a7` |
| 117..=216 | regtest | `617a4a7bb93b55a3959719866b098934dc8c121d22c3a53ee6e07b2023c353f1` | `35670fabfabea438564e1a15bdd648ce0b4bb12b6e6718722b23cc8862659e94` |
| 117..=1116 | regtest | `617a4a7bb93b55a3959719866b098934dc8c121d22c3a53ee6e07b2023c353f1` | `451d13035bd856bc99440eab69d6861d4a233ad4359d2d76b9217778c9234a0c` |

## 6. 読み取れること

### 6.1 規模に対する伸び（T06）

- `scan` の 1 ブロックあたりの時間（§4.1）は、100 ブロックと 1000 ブロックでほぼ同じ（`fixture` 0.401 → 0.402 ms、`core` 1.323 → 1.211 ms、`oracle-v2` 1.580 → 1.620 ms）。時間はブロック数にほぼ比例する。
- 10 ブロックでは 1 ブロックあたりが大きい（`core` 1.741 ms）。接続とヘッダ検証の固定費が効くため。
- `core` / `oracle-v2` は**接続のたびに約 90 KB を必ず取得する**（§4.3 の `sync_noop bytes`: 90,466 / 90,627 バイト。範囲に依らず一定）。
  genesis から先端（高さ 1116）までのヘッダ鎖を検証するためで、「ノードが言うチェーン名を信じず、genesis からの連結を確かめる」（DECISIONS D-029）ことの対価である。
  1 ヘッダは 80 バイトなので、チェーンが長いほど増える。**ヘッダ鎖を永続化しない現在の実装は、長いチェーンでは接続のたびにこの費用を払う**（未対応。O-016）。

### 6.2 Core 直結と Oracle 経由（同じチェーン、同じ範囲。§4.2）

| 1000 ブロックの `scan`（§3） | `core` | `oracle-v2` | 比（§4.2） |
|---|---|---|---|
| wall（ms） | 1211.43 | 1620.24 | 1.34 |
| クライアントの CPU（ms） | 950 | 610 | 0.64 |
| 受信バイト数 | 1,042,331 | 851,386 | 0.82 |
| リクエスト数 | 2022 | 1023 | 0.51 |

- **この条件では、Oracle 経由は速くならなかった。** `scan` の wall の比は 10 / 100 / 1000 ブロックで 1.16 / 1.19 / 1.34。
- **この比は安定していない。** 同じ日・同じ機械での 1 回前の計測（commit `d5c6149` の `docs/benchmarks/`）では、1000 ブロックで 1.07 だった。
  計測セッション間の差は、1 セッション内のばらつき（§3 の spread）より大きい。言えるのは「速くならなかった」までで、「何割遅い」とは言えない。
- クライアントの CPU 時間は Oracle 経由の方が少ない（100・1000 ブロックで 0.60・0.64 倍）。ただし `oracle-v2` では wall 1620 ms のうちクライアントの CPU は 610 ms で、
  残りはクライアントが計算していない時間である。その内訳（サーバの処理、通信、待ち）は**測っていない**。
- `core` では wall 1211 ms のうち 950 ms がクライアントの CPU である。この条件では、Core 直結の時間の大半はクライアント自身の処理（デコード、構造検査、スキャン）に使われている。どの処理にどれだけかかるかは分離していない。
- CPU が減る理由として考えられるのは、入力の検査と tweak の計算をサーバが済ませていることだが、**これは推測で、測って確かめてはいない**。その分の計算はサーバ側に移っただけで、サーバ側の費用は取得できない
  （参考: Oracle の `/info` が先端の高さ 1116 を報告するまで、起動を含めて約 0.76 秒。これがインデックス完了までの時間を正しく表すかは確認していない: UNVERIFIED）。
- 受信バイト数の差は小さい（1000 ブロックで 0.82 倍）。regtest のブロックには受信者に関係のない tx がほとんど無く、フルブロックでも小さいため。
  mainnet のように大半の tx が無関係なチェーンでは差が開くと予想されるが、**この計測からは言えない**（UNVERIFIED）。
- リクエスト数はほぼ半分（0.51 倍）。Core 直結はブロックあたり 2 回（ブロックと spent outputs）、Oracle 経由は 1 回。往復遅延のある環境では効くはずだが、測っていない。

### 6.3 永続化の費用と再開（§4.3）

- `sync_cold − scan`（新しい SQLite ストアを作り、チャンクごとにコミットする費用）は、10 ブロックで 17.65〜20.33 ms、1000 ブロックで 33.14〜172.38 ms。
  比にすると 1000 ブロックで 1.02〜1.23 倍。バックエンド間のばらつきが大きく、コミット 1 回あたりの費用は分離できていない。
- `sync_resume`（後半だけ）は `sync_cold` の 0.43〜0.56 倍（100・1000 ブロック）。スキャン済みの範囲を取り直していない（取り直さないこと自体は、同期スイートの `syncing_again_changes_nothing` が検査している）。
- `sync_noop` は `core` / `oracle-v2` では範囲に依らずほぼ一定（約 6〜9 ms、5〜6 リクエスト）。`fixture` で範囲に比例して増える（1000 ブロックで 58 ms）のは、毎回 fixture ファイル全体を読み込んで検査しているためで、同期処理の費用ではない。
- `restore`（ストア削除 → 同じ birthday から再構築）は `sync_cold` の 0.95〜1.04 倍（100・1000 ブロック）で、**削除前のストアと検出結果が一致**した（全 60 回。一致しなければその run は失敗として記録される: テスト `a_restore_that_differs_from_the_deleted_store_is_a_failed_run`）。

### 6.4 `local-index` について

`local-index` は「フルブロックを取得し、クライアント内でインデックスを計算してからスキャンする」もので、数値は `fixture` とほぼ同じになる（§4.2 の比は 1000 ブロックで 0.92〜1.01）。
インデックスサーバを使った場合のクライアント費用の見積もりには**ならない**。意味があるのは「同じデータから 2 通りの経路で同じ結果が出る」ことの確認である。

### 6.5 ばらつき

- 60 組（バックエンド × モード × 範囲）の spread（(最大 − 最小) / 中央値、§3）は、中央値で 22%、最大は 95%（`local-index` / `sync_noop` / 100 ブロック）。
- spread が 50% を超えるのは 6 組で、いずれも数 ms〜20 ms 程度の短い run である。短い run は数 ms の揺れがそのまま数十 % になる。CPU 時間は分解能 10 ms のため、短い run では 0 か 10 になる。
- 6.2 のとおり、計測セッション間の差はこれより大きいことがある。

## 7. この結果から言えないこと

1. **インターネット越しの性能**。すべてループバック。
2. **mainnet 規模**。ブロックの大きさ・tx 数が桁違いに小さい。1 ブロックあたりの時間・バイト数を mainnet に外挿してはならない。
3. **インデックス方式の総コスト**。サーバ側の前処理・保存・運用の費用は含まれていない。
4. **時間の内訳**。通信待ちと計算、検査とスキャンを分離していない。
5. **数十万ブロックの同期**。最大 1000 ブロック。ヘッダ鎖の検証費用（6.1）は長いチェーンで支配的になり得る。
6. **サーバのキャッシュが冷えた状態**。ノードの再起動や OS のページキャッシュ破棄はしていない。
7. **他の機械・他の日での絶対値や比**。共有の仮想マシン 1 台、1 日の結果で、同じ日の 2 回の計測でも Oracle / Core の比は 1.07 と 1.34 に分かれた。
8. **正しさの証明**。全 run の結果が一致したことは、経路どうしが整合していることを示すだけで、結果が完全であることの証明ではない（`FAULT_INJECTION.md`）。
   なお実ノードのチェーンでは、送信側が作った 100 出力（`bench_node.json` の `expected_detections`）と検出数 100 が一致している。

## 8. 再現

`BENCHMARK_PROTOCOL.md` §5 のコマンド。このレポートの表は次で生成した。

```bash
target/release/silent-sync export --input offline_10.json --input offline_100.json --input offline_1000.json \
    --input node_10.json --input node_100.json --input node_1000.json --format markdown
```
