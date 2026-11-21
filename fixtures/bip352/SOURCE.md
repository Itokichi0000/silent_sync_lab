# BIP352 公式テストベクトル

| 項目 | 値 |
|---|---|
| ファイル | `send_and_receive_test_vectors.json` |
| 取得元 | https://github.com/bitcoin/bips/blob/master/bip-0352/send_and_receive_test_vectors.json |
| 取得時の bitcoin/bips HEAD | `fdcd8024595bc1e97db73883e716dbb91627c371`（2026-11-20） |
| BIP352 バージョン | 1.1.2（2026-10-25） |
| SHA-256 | `23cc7c3b496557970b34c0d820d412260ee1cc1f6046b2d5c0c7a016de88706c` |
| ベクトル数 | 29（受信ケースは30） |
| ライセンス | BSD-2-Clause（BIP352 本文の License ヘッダ） |

- 無改変でコピーしている。`crates/sync-core/tests/bip352_vectors.rs` が SHA-256 と件数を検査するので、差し替えると必ずテストが落ちる。
- 含まれる秘密鍵は BIP が公開しているテスト用の値であり、誰でも読める。**この鍵から導出したアドレスに価値のあるものを送ってはならない。**
- ラボの固定テスト受取人（`lab-cli`）は、このファイルの先頭ケース（`[0].receiving[0]`）の鍵をそのまま使う。リポジトリ独自の秘密鍵は一切追加していない。
- 更新手順: 上流の現行ファイルを取得 → この表と `bip352_vectors.rs` の `VECTORS_SHA256` / `VECTOR_COUNT` を更新 → 追加ケースが通ることを確認 → `docs/DECISIONS.md` に理由を記録。
