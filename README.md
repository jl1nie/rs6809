# rs6809

Pure RustのMotorola 6809エミュレータです。外部crate・Cコード・FFIを使わず、標準ライブラリのみで動作します。
64KBの物理RAMとACIA相当のシリアルコンソールを備え、サンプルとしてNitrOS-9 Level 1とBASIC09を起動できます。

## ビルドと最小サンプル

Rust edition 2024対応のツールチェーンを使用します。

```sh
cargo build --release
cargo run --example hello
cargo test
```

`hello`は6809の機械語を実行し、メモリマップされたACIA経由で `Hello, 6809!` を出力します。

## NitrOS-9 / BASIC09

[kyo-ta04/nitros9-basic09-starter-kit](https://github.com/kyo-ta04/nitros9-basic09-starter-kit) の起動ROMとSDイメージを利用します。
検証したコミットは `bf60370cfa180636cde3ee305d089d94872de82a` です。
取得済みならクローンは不要です。参照キットのCエミュレータやOSをビルドする必要はありません。

```sh
git clone --recursive https://github.com/kyo-ta04/nitros9-basic09-starter-kit.git reference/nitros9-basic09-starter-kit
git -C reference/nitros9-basic09-starter-kit checkout bf60370cfa180636cde3ee305d089d94872de82a
cargo run --release -- \
  --rom reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin \
  --disk reference/nitros9-basic09-starter-kit/nitros9-runtime/multicomp09_sd.img \
  --boot
```

取得・ビルド後は `sh examples/nitros9.sh` でも同じ構成で起動できます。

`--boot`はCamelForthで `NITROS9` を入力します。`Time ?` に `2026/10/30 12:00:00` のように入力すると `OS9:` が表示されます。
`basic09` を実行すると `B:` プロンプトに入ります。
標準入力はホストの端末設定に従います。通常の端末では行を入力してEnterで送信します。終了はCtrl-Cです。

自動でBASIC09プログラムを編集・実行し、`42` を出力する例：

```sh
cargo run --release -- \
  --rom reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin \
  --disk reference/nitros9-basic09-starter-kit/nitros9-runtime/multicomp09_sd.img \
  --boot --script examples/basic09.tsv --until 42 --max-cycles 300000000
```

スクリプトの各行は `待機する出力<TAB>入力文字列` です。入力末尾にCRを付け、仮想CPUのサイクル数に合わせて1文字ずつ送信します。
自動実行中は標準入力に文字を送らないでください。

実機コードを使用した起動テスト：

```sh
cargo test --release --test nitros9 -- --ignored --nocapture
```

このテストはROM→NitrOS-9→シェル→BASIC09→編集モード→`RUN test`→`42`→`B:` の全経路を確認します。
参照キットが必要なので通常の `cargo test` では無視されます。

## Cコンパイラ

Microware Cの各段階・ヘッダ・標準ライブラリと、64KB向けの小さな `cc` ドライバを追加したディスクを使用します。
元のBASIC09用イメージは保持します。C開発用イメージの作成：

```sh
python3 scripts/build-c-disk.py
```

初回の構築にはPython 3.12以降・make・ホストのCコンパイラ・curlが必要です。
LWTOOLS 4.25を取得してSHA-256を確認し、LWASMと参照キットのToolShedを作業ディレクトリ内にビルドします。
システムへのインストールは行いません。既存のツールを使う場合は `LWASM` と `OS9_TOOL` に実行ファイルのパスを指定できます。
ホストのCコンパイラはこれらの構築に使用します。ゲスト側のCのコンパイルは6809上で実行します。

```sh
sh examples/nitros9-c.sh
```

日時を入力した後、OS9のシェルで：

```text
chd /dd/SOURCES
cc hello
./hello
```

`Hello from C: 42` と表示されます。サンプルは `examples/hello.c` です。
K&R形式のCを使い、関数・ループ・整数演算と `printf` を実行します。現代のISO Cとは構文や標準ライブラリが異なります。

`cc hello` または `cc hello.c` は現在のディレクトリにある単一ソースを対象に、次の順に起動します。

```text
c.prep → c.pass1 → c.pass2 → c.opt → c.asm → c.link
```

`cc` はファイル名のみを受け取り、パス指定や複数ソース、追加オプションには対応していません。
出力実行ファイルは同じディレクトリの `hello`、中間ファイルは `hello.m/.i/.a/.o/.r` です。
各段階が失敗するとそこで停止します。Microwareの元の `cc1` と `make` も収録していますが、
この64KB構成では `cc1` 経由の実行でメモリ不足が発生したため、検証済みの `cc` を使用してください。

前処理器は `/dd/DEFS`、リンカは `/dd/LIB/cstart.r` と `/dd/LIB/clib.l` を使用します。
ホストでサンプルソースを変更した場合はディスク作成スクリプトを再実行してください。
ディスク再構築はC開発用イメージを初期状態に戻します。ゲストで作成した実行ファイルや変更は、現在の仕様では終了時に消えます。

自動でコンパイル・実行する場合：

```sh
sh examples/nitros9-c.sh --script examples/ccompiler.tsv --until 'Hello from C: 42' --max-cycles 300000000
```

Cのコンパイル、再コンパイル、失敗後のシェル復帰のテスト：

```sh
cargo test --release --test ccompiler -- --ignored
```

## マンデルブロー集合のCデモ

リポジトリのルートで、エミュレータのコマンド作成、C開発用ディスク作成、
ゲスト内でのCベンチマークのコンパイル・実行を順に行います。
事前に上記の参照キット取得と「Cコンパイラ」の必要ツールの準備を済ませてください。

1. ホストでエミュレータのコマンドを作成します。

   ```sh
   cargo build --release
   ```

   実行ファイルは `target/release/rs6809` です。

2. CソースとMicroware Cを収録したディスクを作り、エミュレータを起動します。

   ```sh
   python3 scripts/build-c-disk.py
   sh examples/nitros9-c.sh
   ```

   `Time ?` に `2026/10/31 12:00:00` のように日時を入力します。

3. ゲストの `OS9:` プロンプトでCベンチマークをコンパイルします。

   ```text
   chd /dd/SOURCES
   cc mandel
   ```

   `c.prep` から `c.link` まで実行され、同じディレクトリに実行コマンド `mandel` ができます。

4. ゲストで実行します。

   ```text
   ./mandel
   ```

   79列×25行のマンデルブローが描画され、`MANDELBROT-DONE` と表示されます。
   対話実行の終了はCtrl-Cです。

ディスク作成からコンパイル・実行・1回の速度計測まで自動で行う場合は：

```sh
sh examples/mandelbrot.sh
```

標準エラーの `measured_cycles` が描画区間の仮想CPUサイクル数、続く `host_seconds` が
ホストでの所要秒数です。起動・コンパイル時間はこの描画区間に含みません。

ウォームアップ1回と計測7回で中央値を求める場合は、ディスク作成後に：

```sh
python3 scripts/benchmark-emulator.py --workload basic09 --runs 7 --output artifacts/benchmark/mandelbrot-c.json
```

こちらはビルド済みの `target/release/rs6809` を使います。
結果は画面と指定したJSONに保存します。Cソースを変更した場合はディスクを作り直してください。

画像で指定されたBASIC09の `PROCEDURE mandel` をCへ直接移植しています。
79列×25行、X=-39〜39、Y=-12〜12、実数演算、最大16回反復で、発散した点を0〜F、内部を空白で表示します。
初期値z=c、更新後の発散判定も元のBASIC09に合わせています。
ソースは [examples/mandel.c](examples/mandel.c)、自動入力は `examples/mandelbrot.tsv` です。
元のBASIC09とC版を実行して比較し、**1,975文字すべての一致**を確認しました。
この実数版の描画時間は7回の中央値 **1.673355秒**、704,530,641仮想サイクル（421.0MHz相当）でした。
起動・コンパイルを除いた計測で、[詳細と再現手順](docs/mandelbrot-basic09-port.md#c移植版の速度ベンチマーク)を保存しています。

元のBASIC09ソースも `/dd/SOURCES/mandel.b09` に収録しています。
再比較は `python3 scripts/compare-basic09-port.py` で実行できます。
詳しい移植対応とSDドライバの修正は [移植の記録](docs/mandelbrot-basic09-port.md) を参照してください。

以前の76×25・固定小数点版は `benchmarks/mandel_fixed.c` として保存しました。
エミュレータの性能比較は凍結した旧版のディスクで続け、現在のデモとは混ぜません。

## 第0版と性能計測

現状を第0版（v0）としてソース・実行ファイル・入力ハッシュとともに保存しています。
マンデルブローを7回測定した中央値は1.481785秒（331.2MHz相当）でした。
最大のホットスポットはメモリ読み取りで、描画後半の約34%です。
詳細と改善順序は [第0版の性能調査](docs/v0-performance.md)、機械可読の記録は [v0-baseline.json](docs/v0-baseline.json) を参照してください。

```sh
python3 scripts/benchmark-emulator.py
```

既存のrelease実行ファイルとC開発用ディスクを使い、ウォームアップ後に7回測定します。
既定は現在のBASIC09移植版です。
結果は `artifacts/benchmark/current.json` に保存します。
過去の測定と比較する場合は `--workload fixed` を指定します。このモードは
`benchmarks/fixed-inputs.json` に記録した、この作業環境に保存済みの旧固定小数点版の入力が必要です。

## Rust向けの最適化

releaseビルドはopt-level=3、ThinLTO、codegen-units=1です。
通常のメモリアクセスを小さな関数に分け、I/O処理を別関数へ出しています。
さらに、MMU使用時のアドレス変換と保護属性を事前計算し、その表を別オブジェクトへ分けました。

固定小数点版を前後交互に7回測定した中央値は0.972秒→0.878秒で、今回の変更により約1.11倍になりました。
通常RAM・MMU・シリアル出力の別負荷も比較しています。
最新の調査は [実装パターンの再検証](docs/pattern-validation.md)、前回の段階は [Rust最適化の記録](docs/rust-optimization.md) を参照してください。

## 構成

| 項目 | 標準マシン | NitrOS-9用Multicomp互換モード |
|---|---|---|
| 物理RAM | 64KB | 64KB |
| 起動ROM | なし | `$E000–$FFFF`、8KB、無効化可能 |
| ACIA status/control | `$8018` | `$FFD0` |
| ACIA data | `$8019` | `$FFD1` |
| タイマー | `$8010` | `$FFDD` |
| SD data/control/LBA | なし | `$FFD8–$FFDC` |
| マッピング制御 | なし | `$FFDE–$FFDF` |

- CPU：6809の定義済み命令、各アドレッシング、CC、IRQ/FIRQ/NMI、SWI/SWI2/SWI3、SYNC/CWAI、命令サイクル加算。
- ACIA：受信キュー、送信出力、受信・送信IRQ、マスターリセット。送信は即時完了です。
- タイマー：既定20,000サイクルごと。1MHz換算で50Hz。実時間への速度制限はありません。
- SD：512バイト単位。書き込みはメモリ内の差分に保持し、終了すると消えます。
- MMU：8KBバンクの制御を扱いますが、物理RAMは64KBです。未搭載バンクは `$FF` を返します。512KB構成やLevel 2の動作は対象外です。
- エミュレーションは命令単位です。バスサイクル単位の精密タイミング、未定義命令、6309拡張、ACIAのボーレート・パリティ・エラーフラグは扱いません。

任意のバイナリをRAMにロードする場合：

```sh
cargo run --release -- --bin program.bin --load 0x1000 --pc 0x1000 --max-cycles 1000000
```

ライブラリからは `Cpu` と `Bus` を使ってCPUだけを組み込めます。`Machine` はRAM・コンソール・タイマー付きの実装です。
命令エラーとディスク読み込みエラーは実行を失敗として報告します。
`--until` は出力文字列への到達で成功終了し、到達前にサイクル上限に達すると失敗します。

## ライセンスと参照

rs6809のコードはMITライセンスです。`reference/` は本体の配布対象外で、取得したキット、ROM、OS、BASIC09にはそれぞれの権利・ライセンスが適用されます。

CPU仕様：[Motorola MC6809/MC6809E Programming Manual](https://www.maddes.net/m6809pm/index.htm)。
Multicompのレジスタ仕様と起動手順は参照キットの `exec09/miscsbc.c`、`nitros9-mc09-build/defs/mc09.d`、`nitros9-runtime/` を確認しています。
