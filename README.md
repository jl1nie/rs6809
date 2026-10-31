# rs6809

Pure RustのMotorola 6809エミュレータです。外部crate・Cコード・FFIを使わず、標準ライブラリのみで動作します。
標準構成は64KB RAMとACIA相当のシリアルコンソールです。Multicomp互換モードは既定512KB・最大1MBのRAMを備え、NitrOS-9 Level 1 / BASIC09とLevel 2を起動できます。
Level 2では1つのOSを共有してTelnetから複数ユーザー・複数プロセスを同時に動かせます。

## ビルドと最小サンプル

Rust 1.89以降（edition 2024対応）のツールチェーンを使用します。

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

## NitrOS-9 Level 2（512KB／1MB）

上記と同じ参照キット（コミット `bf60370cfa180636cde3ee305d089d94872de82a`）から、
Multicomp用Level 2カーネルと専用ディスクを構築できます。
Python 3.12以降・make・ホストのCコンパイラ・curl・patchが必要です。
LWTOOLS 4.25とToolShedは「Cコンパイラ」と同じ手順で作業ディレクトリ内に用意されます。

```sh
python3 scripts/build-level2-disk.py
sh examples/nitros9-level2.sh
```

起動後に `NitrOS-9/6809 Level 2 V03.3.0`、`LEVEL2-STARTED`、`OS9:` が表示されます。
`mfree` で物理メモリの上限と空きメモリを確認できます。既定の512KBでは `$7FFFF` まで、
`--ram-kb 1024` を付けて起動すると `$FFFFF` までを使います。
ディスクには `shell`、`echo`、`del`、`mfree` と、Microware Cのオリジナルのドライバ `cc1` 一式
（`c.prep`〜`c.link`、`/dd/DEFS` のヘッダ、`/dd/LIB` のライブラリ）、BASIC09（`basic09`・`runb`・`inkey`・`syscall`）を収録しています。
BASIC09は参照キットの検証済みBASIC09ディスクから同じモジュールを取り出します。
`/dd/SOURCES` には `hello.c`・`mandel.c`・`mandel.b09` を置きます。

```text
OS9:chd /dd/SOURCES
OS9:cc1 hello.c -f=/dd/SOURCES/hello
OS9:/dd/SOURCES/hello
Hello from C: 42
```

`cc1` は作業用のシェルスクリプト `c.com` と中間ファイル `ctmp.<プロセス番号>.*` を現在のディレクトリに作ります。
`-f=` を省略すると実行ファイルを実行ディレクトリ `/dd/CMDS` に出力します。

```text
OS9:chd /dd/SOURCES
OS9:basic09
B:load mandel.b09
B:run mandel
```
Level 2では各プロセスが専用の64KB空間を持つため、Level 1でメモリ不足になった `cc1` も動作します。
Level 2用の参照ソースに必要な変更は生成先だけに適用し、参照キットとLevel 1ディスクは保持します。
変更理由は [Level 2対応の記録](docs/level2.md) を参照してください。

```sh
cargo test --release --test nitros9_level2 -- --ignored
```

このテストはROMからの起動、startup実行、シェルでの `mfree`、別プロセスの `echo` 実行と
シェルへの復帰、`cc1` による `hello.c` のコンパイル・実行、BASIC09での `6*7` と `mandel.b09` の実行を確認します。

MulticompのRAMは既定512KBで、`--ram-kb 64|128|256|512|1024` で選べます。
従来の64KB構成は `--ram-kb 64` で再現できます。
Level 2カーネルは起動時に512KBか1MBかを検出するため、同じディスクをどちらでも使えます。
Level 2には512KB以上が必要です。起動スクリプトにも `--ram-kb 1024` を追加できます（例：`sh examples/nitros9-telnet.sh --ram-kb 1024`）。
CoCo 3専用のROM・ディスク・GIMEには対応しません。

## Telnet・マルチユーザー

OS-9 Level 2の端末設定に合わせて同時1〜4セッションを利用できます。
各接続を空きACIAへ割り当て、OS-9の `login` で認証します。プロセスとSIGHUP処理もOS-9が管理します。
待ち受けはlocalhost限定、TCPポート番号は空き番号を自動取得します。Telnetモードのファイル書き込みは永続化します。

```sh
python3 scripts/build-level2-disk.py --multi-user --acia-count 3
sh examples/nitros9-telnet.sh
```

起動時に表示された `127.0.0.1:ポート番号` に別の端末から接続し、Enterを押してログインします。
例えば表示が `127.0.0.1:49152` なら：

```sh
telnet 127.0.0.1 49152
```

初期構成は `/term`・`/t0`・`/t1` の3端末です。構築時の `--acia-count 1|2|3|4` で、
OS-9の端末記述子と `tsmon` の起動数を変更できます。準備できた端末だけを空きACIAとして受け付け、
同時接続数はOS-9側の設定で決まります。Telnetモードではホストの標準入力は使いません。

初期アカウントは `user1`〜`user3` で、ランダムな初期パスワードを
`artifacts/level2-multiuser/accounts.txt` に保存します。認証はOS-9の `login` が行い、
切断はACIAのDCD消失として通知します。OS-9のドライバと既存のSIGHUPハンドラが後処理を行います。

ファイルの保存先は `artifacts/level2-multiuser/multicomp09-level2.img` です。
コマンド完了後の変更はエミュレータ再起動後も残ります。同じイメージへの二重書き込みは排他ロックで防ぎます。
既存のマルチユーザーディスクがある場合、構築スクリプトは再構築を拒否します。
`--force` を指定した再構築は保存ファイルとアカウントを初期化します。

実際のOS-9で同時ログイン・接続上限・セッション分離・再接続・ファイル永続化を検証するには：

```sh
python3 scripts/test-telnet.py
python3 scripts/test-telnet.py --acia-count 4
```

テスト用ディスクは一時ディレクトリに作成します。
設定・切断時の動作・保存先・検証手順は [Telnet接続の説明](docs/telnet.md) にまとめています。

### 複数ユーザーで同時にマンデルブロー

複数のTelnetユーザーが1台の6809を共有し、それぞれのホームで `mandel.c` を `cc1` でコンパイルして実行します。
参照キットと「Cコンパイラ」の必要ツール、`cargo build --release` を事前に済ませてください。

```sh
python3 scripts/demo-mandelbrot.py --users 3
```

スクリプトは `artifacts/level2-mandel/` にマルチユーザーディスクを作り直し、次の順に実行します。

1. 1人で `cc1 mandel.c -f=/dd/ユーザー名/mandel` と `/dd/ユーザー名/mandel` を実行し、基準の画像と時間を記録します。
2. 全員が同時にコンパイルします。
3. 全員が同時に自分の `mandel` を実行し、画像が基準と一致することを確認します。

端末では各ユーザーのコンパイル段階と描画済み行数を表示し、最後に単独時と同時実行時の所要時間を比較します。
`--users 1|2|3|4` で人数を、`--skip-build` で既存ディスクの再利用を指定できます。
マルチユーザー構成では各ユーザーのホームに `hello.c`・`mandel.c`・`mandel.b09` を置きます。
出力先は `-f=` で自分のホームを指定してください。省略すると全員共通の `/dd/CMDS` に書き込みます。

手動で試す場合は、デモでディスクを作成した後に起動し、別々の端末から接続します。

```sh
sh examples/nitros9-mandel-telnet.sh
telnet 127.0.0.1 ポート番号
```

アカウントは `artifacts/level2-mandel/accounts.txt` にあります。
エミュレータの標準出力には起動ログだけを表示するため、同じ端末でtelnetを使っても出力は混ざりません。

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

## ICE（GUIデバッガ）とOS-9モニタ

ブラウザから操作するインサーキット・エミュレータ相当のデバッガです。外部crateを使わず、エミュレータ自身がlocalhostでHTTPを提供します。

```sh
sh examples/nitros9-ice.sh            # NitrOS-9 Level 1 → http://127.0.0.1:6809/
sh examples/nitros9-level2.sh --ice   # Level 2（ディスク構築後）
```

- **デバッガ**：実行・停止・ステップ・ステップオーバー／アウト・その行まで実行、ブレークポイント、ウォッチポイント、
  逆アセンブル（`OS9 F$Fork` 形式）、メモリ（CPU論理・OS-9システム空間・物理）、レジスタ編集、MMUマップ、実行トレース、速度上限。
- **OS-9モニタ**：すべての `OS9` システムコールを呼び出しから復帰まで追跡し、引数・結果・所要サイクルを表示します。
  各プリミティブの日本語の説明、カーネル内部で発生した呼び出しの木、スケジューラのキューとタイムライン、プロセス表、
  モジュールディレクトリ、メモリマップを表示し、「F$Forkの呼び出しで停止」「エラー復帰で停止」などをICEのブレーク条件として設定できます。
- **シンボルとソース行**：LWASM・Microware RMAのリストとリンクマップ（`--symbols`）から逆アセンブルとスタックにラベルを付け、
  Cやアセンブリのソース行でブレーク・ステップできます。OS-9のモジュールはロードされた位置へ自動で配置します（Level 1／2）。
  Cは `python3 scripts/c-debug-build.py examples/hello.c` でゲスト上で行情報付きにコンパイルします。
- **エディタ連携**：`--dap` でDebug Adapter Protocolを提供し、VS Code（`editors/vscode`）などから同じICEを操作できます。

詳細・仕組み・APIは [ICEとOS-9モニタ](docs/ice.md) を参照してください。

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
| 物理RAM | 64KB | 既定512KB（64KB〜1MBを選択可） |
| 起動ROM | なし | `$E000–$FFFF`、8KB、無効化可能 |
| ACIA status/control | `$8018` | `$FFD0`、追加端末は `$FFD2`・`$FFD4`・`$FFD6` |
| ACIA data | `$8019` | `$FFD1`、追加端末は `$FFD3`・`$FFD5`・`$FFD7` |
| タイマー | `$8010` | `$FFDD` |
| SD data/control/LBA | なし | `$FFD8–$FFDC` |
| マッピング制御 | なし | `$FFDE–$FFDF` |

- CPU：6809の定義済み命令、各アドレッシング、CC、IRQ/FIRQ/NMI、SWI/SWI2/SWI3、SYNC/CWAI、命令サイクル加算。
- ACIA：最大4台。受信キュー、送信出力、受信・送信IRQ、DCD、マスターリセット。送信は即時完了です。
- タイマー：既定20,000サイクルごと。1MHz換算で50Hz。実時間への速度制限はありません。
- SD：512バイト単位。Telnetモードまたは `--persist-disk` で永続化し、排他ロックします。その他の既定モードと `--volatile-disk` はメモリ内の差分を使用します。
- MMU：8KBバンク、2組の独立したタスクマップ、バンクごとの書き込み保護。未搭載バンクは `$FF` を返し書き込みを無視します。固定上位領域 `$FE00–$FFFF` は物理バンク7を共有し、I/Oを除き常に読み書き可能です。
- エミュレーションは命令単位です。バスサイクル単位の精密タイミング、未定義命令、6309拡張、ACIAのボーレート・パリティ・エラーフラグは扱いません。

任意のバイナリをRAMにロードする場合：

```sh
cargo run --release -- --bin program.bin --load 0x1000 --pc 0x1000 --max-cycles 1000000
```

ライブラリからは `Cpu` と `Bus` を使ってCPUだけを組み込めます。`Machine` はRAM・コンソール・タイマー付きの実装です。
`Machine::multicomp()` は512KB、`Machine::multicomp_with_ram(rom, disk, ram_bytes)` は指定サイズのRAMを用意します。
公開フィールド `ram` は物理メモリの `Box<[u8]>` です。`load()` は従来どおり下位64KB内への直接ロードです。
命令エラーとディスク読み込みエラーは実行を失敗として報告します。
`--until` は出力文字列への到達で成功終了し、到達前にサイクル上限に達すると失敗します。

## ライセンスと参照

rs6809のコードはMITライセンスです。`reference/` は本体の配布対象外で、取得したキット、ROM、OS、BASIC09にはそれぞれの権利・ライセンスが適用されます。

CPU仕様：[Motorola MC6809/MC6809E Programming Manual](https://www.maddes.net/m6809pm/index.htm)。
Multicompのレジスタ仕様と起動手順は参照キットの `exec09/miscsbc.c`、`nitros9-mc09-build/defs/mc09.d`、`nitros9-runtime/` を確認しています。
