# FUZIX/68000とプロセスモニタ

FUZIX 0.5（Alan Cox氏らによる小さなUnix系OS）のTiny68K版を、Tiny68K互換の機械（[68000 CPUコア](m68k.md)）で動かし、
ICEでシステムコール・プロセス表・プロセス切り替えを観察します。OS-9との比較は [fork・exec・waitとOS-9のF$Fork・F$Wait](fork-compare.md) にあります。

```sh
sh scripts/build-m68k-elf.sh          # 初回だけ。m68k-elfのbinutilsとgcc（数十分）
sh scripts/build-fuzix-tiny68k.sh     # FUZIXのカーネル・コマンド・ディスク
sh examples/fuzix68k.sh               # rootでログインしたシェル（^Cで終了）
sh examples/fuzix68k.sh --ice         # ICE: http://127.0.0.1:6809/
```

```text
FUZIX version 0.5
...
Motorola 68000 processor detected.
16352KiB total RAM, 16254KiB available to processes (125 processes max)
Enabling interrupts ... ok.
IDE drive 0: hda: hda1 hda2
bootdev: hda1
...
login: root

Welcome to FUZIX.
# ps
  PID    TTY     TIME CMD
    9    tty1 00:00:22 -sh
   11    tty1 00:00:00 ps
```

## ツールチェーン

FUZIXの68000版は `m68k-elf-gcc -m68000` を前提にし、ユーザープログラムを68000用のlibgcc（32ビットの乗除算の補助関数）とリンクします。
FUZIXの `README.68000.md` は、m68k-linux-gnuのgccを使わないよう警告し、作者の環境としてgcc 10.3.0（`--with-cpu=68000`）を挙げています。
`scripts/build-m68k-elf.sh` は、binutils 2.42とgcc 10.3.0（Cのみ、`--with-cpu=68000`）をGNUの配布元から取得し、SHA-256を確かめて
`artifacts/tools/m68k-elf` に組み立てます。sudoは要りません。組み立てたlibgccに68020以降の命令がないことを、逆アセンブルで確かめました。

`scripts/build-fuzix-tiny68k.sh` は、`reference/FUZIX`（コミット `776bfc9`）で `make TARGET=tiny68k diskimage` を実行し、
`artifacts/fuzix68k` に `disk.img`・`fuzix.elf`・`fuzix.sym`（nmの出力）・`offsets.txt` を置きます。手を入れたのは2か所です。

- `Applications/as09` の `as.c`：gcc 10.3の `-Os` が `switch` の表を誤った大きさにする（`value -684 out of range`。README.68000.mdがgcc 11の問題として書いているもの）ため、
  `-fno-jump-tables` で先に作ります。
- `Applications/MWC/cmd`：2つの文法を同じ `y.tab.c` に出すため、並列のmakeでは一方の出力がもう一方の `.c` になります。ここだけ並列にせず先に作ります。

## 起動

モニタROMはありません。`--boot-sector` で、エミュレータがディスクのLBA 1（512バイト）を `$15000` に置いて始めます。
モニタの「CP/M起動ローダー」と同じ動きで、起動ローダー（`loader.S`、`$15000` 向け）が `$1F0000` に移ってからカーネルをLBA 2以降から `$400` に読み、起動します。
ディスクは基板と同じくバイトの並びが入れ替わっているので、ブートセクタも語ごとに入れ替えて置きます。

- 時計：カーネルがDUARTのタイマー（初期値 `$480`、ベクタ `$81`）と割り込みマスクを設定し、端末の初期化でACRにタイマーモード（`$70`／`$F0`）を書きます。
  エミュレータのACRはリセット時の0のままで、100 Hzの時計が動きます。
- IDEのドライブ1（スレーブ）はありません（DEVビットが立つと状態0）。カーネルは `IDE drive 1: ide error, status=0000` と表示します。
- 起動時の質問（`bootdev:`、日付、時刻、`login:`）には `examples/fuzix68k/boot.script` が答えます。
  スクリプトでは `#` で始まる行は注釈なので、プロンプト `# ` を待つ行は `\#` と書きます。

## FUZIXモニタ（`src/ice/fuzix.rs`）

`--os fuzix` で使うOSの層です。カーネルのシンボル（`--symbols artifacts/fuzix68k/fuzix.sym`）から `udata_shadow`・`ptab`・`ptab_end`・`e_trap12` を受け取り
（`OsMonitor::use_symbols`）、TRAP #12のベクタが `e_trap12` を指し、プロセスが動いていれば有効になります。

| 項目 | 内容 |
|---|---|
| 捕捉 | 各プログラムの先頭に写される入口の `TRAP #12`。番号はD0.W、引数はD1・A0・A1・A2（`e_trap12` が `u_argn` に写す順） |
| 復帰 | D0が結果。エラーならD0＝-1、D1.Wがエラー番号（`ENOENT` など） |
| 名前 | `Kernel/include/syscall_name.h` の80個（`_fork`・`execve`・`waitpid`・`_exit`・`open`・`read`・`write` など）。主なものに日本語の要約と説明 |
| 引数・結果 | パス名、`execve` の引数の並び、ファイル記述子と開いたときのパス名（`fd 9（/dev/tty1）`）、`write` の中身、`read` で読んだ中身、`_fork` の子のpid、`waitpid` のpidと終了コード |
| 実行中のプロセス | `udata_shadow`（実行中のプロセスの `u_data`）。その `u_ptab` から `p_pid` |
| プロセス表 | `ptab` から `ptab_end` が指す終わりまでの使用中の項目：pid、親、名前、状態（実行中・実行待ち・睡眠・I/O待ち・停止・fork中・ゾンビ）、uid、端末、待っているもの、優先度、nice、大きさ、時間 |
| キュー | 実行待ち（`P_READY`）、睡眠（`P_SLEEP`・`P_IOWAIT`）、ゾンビ |
| メモリ | カーネル（`$400`〜`_end`）と、各プロセスの `u_codebase`〜`u_top`、そのプロセスが今システムコールの中か |

構造体の位置（`struct p_tab` は94バイト、`p_pid` は4、`p_pptr` は8、`p_name` は48など）は68000の2バイト境界の詰め方に依存するため、
実際のカーネルのヘッダを `scripts/fuzix-offsets.c` でm68k-elf-gccに読ませて求め、`offsets.txt` とモニタの定数をテストで比べます。

## 検証

```sh
cargo test --release --test fuzix68k -- --ignored
```

- 起動から `ls /`・`ps`・`echo` の出力。シェルの `_fork`（子のpid）、子の `execve "/bin/ps" [ps]`・`open "/dev/proc"`・`_exit`、親の `waitpid` が子のpidを受け取ること、
  `open` で学んだパス名が後の `write` に付くこと、タイムラインが子に切り替わり、シェルに戻ること。
- シェルの子の `execve` の呼び出し前（D0＝23、プロセス表で子は実行中、親のシェルは睡眠）、`waitpid` の復帰後（D0が子のpid）、
  `cat /nosuch` の `open` のエラー復帰（-1 `ENOENT`）で停止。
- DAP：例外ブレークポイントの条件 `_fork`・`waitpid` で止まり、スレッド名が `68000 (FUZIX プロセス9 -sh)` になること。
- モニタの構造体の位置が、カーネルのヘッダから求めた値と一致すること。

## 制約

- 構造体の位置は、このFUZIXの版（`776bfc9`）とTiny68Kの設定のものです。版を変えたら `offsets.txt` とテストで確かめてください。
- プロセスIDは8ビットで扱います（256以上は下位8ビット）。
- ユーザープログラムのシンボルは読み込みません（カーネルのみ）。
- シグナルで中断されたシステムコールは、同じプロセスが次のシステムコールを出した時点で「戻らない」とします。
