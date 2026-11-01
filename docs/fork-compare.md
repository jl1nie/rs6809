# fork・exec・waitとOS-9のF$Fork・F$Wait

同じICEで、NitrOS-9（6809、Level 1）とFUZIX（68000）がシェルからコマンドを動かす様子を記録し、並べたものです。
記録はどちらも `/api/os/calls` の内容で、OS-9は `tests/os_snapshot.rs`（`echo traced` を実行）、FUZIXは `tests/fuzix68k.rs`（`ps` を実行）と同じ手順で取りました。

## 呼び出しの並び

**NitrOS-9**：子を作る呼び出しが、そのまま新しいプログラムを動かします。

```text
#239 P2 F$Link   X→"Shell" 種別A=$11            → ヘッダU=$B800 実行Y=$B843
#240 P2 F$Fork   モジュール"Shell" パラメータ"-P X -p\r" データ3ページ
                                                  → 子プロセスID=3   内部: F$All64, F$AProc
#273 P2 F$Wait   子プロセスの終了を待つ           （子が終わるまで眠る）
#336 P3 F$Fork   モジュール"echo" パラメータ"* NitrOS-9 L1 mc09 / BASIC09 *\r" データ2ページ
                                                  → 子プロセスID=4   内部: F$All64, F$SRqMem, F$AProc
```

**FUZIX**：子を作る（`_fork`）、子が自分の中身を差し替える（`execve`）、親が待つ（`waitpid`）の3つに分かれます。

```text
P9  _fork    フラグ$0                             → 子のpid 11
P9  waitpid  pid -1 状態 $0329B6 オプション0      （子が終わるまで眠る）
P11 execve   "/bin/ps" [ps]                       （成功すると戻らない）
P11 open     "/dev/proc" フラグ$0 モード0         → fd 3
P11 write    fd 1 26バイト "  PID    TTY     TIME CM…"
P11 _exit    終了コード0                          （戻らない）
P9  waitpid  …                                    → pid 11 状態$0000（終了コード0）
```

## 違い

| | NitrOS-9（`F$Fork`・`F$Wait`） | FUZIX（`fork`・`execve`・`waitpid`） |
|---|---|---|
| 子の作り方 | 名前で指定したモジュールを、新しいプロセスとして起動する（forkとexecを1回で行う） | 呼び出したプロセスを丸ごと複製する。子は同じプログラムの続きから動き、`_fork` が0を返す |
| プログラムの指定 | モジュール名（メモリにあればリンク、なければ実行ディレクトリから読む）とパラメータ文字列 | 子が `execve` でパス名と引数の並びを渡し、自分の中身を置き換える |
| 子のメモリ | モジュールのコードは共有し、データ領域だけを新しく取る（`F$SRqMem`、Level 2ではブロックの割り当て） | 平坦なメモリ（MMUなし）なので、親のメモリを別の場所へ丸ごと写す（`pagemap_alloc`）。`execve` で新しいプログラムを読み込む |
| 親に返るもの | 子のプロセスID（Aレジスタ） | 子のpid（D0）。子には0 |
| 待つ | `F$Wait`：どれかの子が終わるまで眠り、子のIDと終了状態（B）を返す | `waitpid(-1, &状態, 0)`：同じく子が終わるまで眠り、pidと状態（上位8ビットが終了コード）を返す |
| 終わり方 | `F$Exit`：状態を親に渡し、プロセス記述子を返す | `_exit`：ゾンビになり、親の `waitpid` で回収される |
| 内部の処理 | カーネル内部のシステムコールとして見える（`F$All64` でプロセス記述子、`F$AProc` で実行待ちへ） | カーネル内部はC関数（`dofork`・`ptab_alloc`・`pagemap_alloc`・`doexit`・`psleep`・`wakeup`）。ICEでは `fuzix.sym` のシンボルでブレークポイントを置いて追う |
| ICEでの捕捉 | `SWI2`＋番号（命令の中に番号） | `TRAP #12`、番号はD0（実行直前のレジスタで決める） |

## ICEで追う

```sh
# NitrOS-9：シェルがコマンドを起動する瞬間
curl -X POST 'localhost:6809/api/os/break?code=F$Fork'
curl -X POST 'localhost:6809/api/os/break?code=F$Wait&when=return'

# FUZIX：fork・execの瞬間と、親が子の終了を受け取る瞬間
curl -X POST 'localhost:6809/api/os/break?code=_fork'
curl -X POST 'localhost:6809/api/os/break?code=execve'
curl -X POST 'localhost:6809/api/os/break?code=waitpid&when=return'
# カーネルの中：子のプロセス表の項目を作るところ（--symbols fuzix.sym）
curl -X POST 'localhost:6809/api/bp?sym=dofork'
```

OSタブのタイムラインでは、FUZIXの `_fork` の後に子（pid 11）へ切り替わり、`_exit` の後にシェル（pid 9）へ戻る様子が帯で見えます。
`execve` で止めたときのプロセス表には、まだ `-sh` という名前のままの子（実行中）と、`waitpid` で睡眠中の親が並びます。
