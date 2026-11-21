# ローカル fixture

| ファイル | 内容 |
|---|---|
| `regtest_basic_v1.json` | Phase 1 用の決定的な regtest 形チェーン（高さ 1〜8、17 tx） |

- 生成器: `crates/sync-core/tests/support/fixture_gen.rs`（**テストデータ生成専用**。上流 `bdk_sp` の送信関数を使う唯一の場所で、`sync-core` 本体と CLI には含まれない）
- 再生成: `cargo run -p sync-core --example gen_fixture -- fixtures/local/regtest_basic_v1.json`
- テスト `committed_fixture_is_exactly_what_the_generator_produces` が、コミット済みファイルと生成結果の完全一致を検査する。

## 何が本物で、何が本物でないか

| 要素 | 状態 |
|---|---|
| ブロックヘッダの連結（高さ1は regtest genesis に接続） | 本物。アダプタが検証 |
| マークルルート / BIP141 witness commitment | 本物。アダプタが検証（commitment は厳密に再計算。witness を剥がしたブロックは拒否） |
| Proof of Work | regtest 難易度で有効。アダプタがヘッダ自身の `bits` に対して検証し、`bits` が正規でネットワーク上限以内であることも確認 |
| BIP34 の高さコミット | 本物。アダプタが検証 |
| トランザクションの署名 | **ダミー**。コンセンサス上は無効 |
| 入力が参照する prevout | **架空**。どのチェーンにも存在しない。ファイル内の prevout と入力の整合も検査していない（`docs/DECISIONS.md` O-010） |
| 金額の整合（入力 ≥ 出力など） | 検証していない |

したがって、この fixture は「受信検出ロジックと同期エンジンの検証」には使えるが、実チェーンについては何も証明しない。計測に使う場合もオフライン・メモリ内の数値であり、ネットワーク越しの性能を示さない。

## シナリオ

| 高さ | 内容 | 設計書のケース |
|---|---|---|
| 1, 4 | coinbase のみ | — |
| 2 | P2WPKH 入力1つ → 受取人宛1出力（+ 無関係な P2TR 出力） | T02 |
| 3 | P2TR + P2WPKH の2入力 → 同一受取人宛2出力（k=0,1、vout 1,2）／別の受取人宛の支払い1件 | T03, T04 |
| 5 | 1 tx 内に ラベル1 (k=0)、change ラベル0 (k=1)、ラベル5 (k=2) | T05 |
| 6 | スキャン対象外3種: 適格入力なし (P2WSH)／SegWit v2 を使う tx／Taproot 出力なし | — |
| 7 | change ラベル単独の支払い | T05 |
| 8 | 1 sat の支払い（少額でも暗号学的に検出する） | — |

`expected_detections` は**送信側の知識**（受取人のために作った出力と、その `k`・ラベル）であり、スキャナの出力から作ったものではない。高さ6の SegWit v2 tx の出力は BIP352 上スキャンしてはならないので、意図的に含めていない。
