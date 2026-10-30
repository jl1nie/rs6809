# 固定した性能比較入力

`mandel_fixed.c` は最初の性能比較に使った固定小数点版（76×25、48反復）。
現在の `examples/mandel.c` は画像のBASIC09を移植した実数版（79×25、16反復）なので別の負荷。

`fixed-inputs.json` は、固定小数点版を含む元のROM・SDイメージ・入力スクリプトとソースのSHA-256を記録する。
バイナリ入力はこの作業環境の `artifacts/benchmark/fixed-inputs/` に保存した。
`benchmark-emulator.py --workload fixed`、`compare-benchmarks.py --workload fixed`、`profile-drawing.py` はこの入力を使用し、ハッシュを確認する。
`--workload basic09`（既定）は通常のビルド済みCデモを使い、凍結入力がなくても計測できる。
デモの更新やSDドライバ修正で性能比較の中身が変わることを防ぐため、自動で作り直したり代用したりしない。
このディレクトリの情報と保存した入力は、比較を続ける間保持すること。
