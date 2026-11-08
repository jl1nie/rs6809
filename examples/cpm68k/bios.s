| CP/M-68K 1.3 BIOS for the Tiny68K-compatible machine of rs6809.
|
| CPM15000.SR (the system without a BIOS, $15000-$1ACC1) calls _init at
| $1B000 on a cold start; BDOS calls the other functions through TRAP #3
| with the function number in D0.W and parameters in D1/D2.
|
|   console   MC68681 DUART channel A ($FFF000, odd addresses); input is
|             waited for with STOP and the receive interrupt (level 2)
|   list/aux  DUART channel B
|   disk A:   IDE (CompactFlash) at $FFE000, LBA, 512-byte sectors
|
| Disk A: is 8 MiB: 2048 tracks of 32 logical 128-byte sectors (8 IDE
| sectors), one reserved track, 4 KiB blocks and 512 directory entries.
| BDOS works in 128-byte sectors; the BIOS keeps one 512-byte IDE sector
| in a buffer (read-modify-write, written through on every write).
|
| Build: python3 scripts/build-cpm68k-bios.py (GNU as for m68k); with --tn20k
| the TPA ends at $800000 for the 8 MiB machine of rs6809-fpga
| (bios-tn20k.sr).

	.equ	CCP, 0x150bc		| _ccp in CPM15000.MAP (warm boot)
	.equ	TRAP3, 0x8c		| TRAP #3 vector
	.equ	TPA_BASE, 0x20000	| TPA above the BIOS ...
	.ifdef	TN20K
	.equ	TPA_END, 0x800000	| ... to the top of the 8 MiB (rs6809-fpga)
	.else
	.equ	TPA_END, 0xfe0000	| ... to near the top of RAM
	.endif

	.equ	DUART, 0xfff000
	.equ	MRA, 0x01
	.equ	SRA, 0x03
	.equ	CSRA, 0x03
	.equ	CRA, 0x05
	.equ	RHRA, 0x07
	.equ	THRA, 0x07
	.equ	ACR, 0x09
	.equ	IMR, 0x0b
	.equ	IVR, 0x19
	.equ	RXVEC, 64		| vector number of the DUART interrupt
	.equ	RXINT_A, 0x02		| IMR: channel A receiver ready
	.equ	SRB, 0x13
	.equ	CRB, 0x15
	.equ	RHRB, 0x17
	.equ	THRB, 0x17
	.equ	RXRDY, 0		| status bits
	.equ	TXRDY, 2

	.equ	IDE, 0xffe000
	.equ	IDE_DATA, 0x00
	.equ	IDE_COUNT, 0x05
	.equ	IDE_LBA0, 0x07
	.equ	IDE_LBA1, 0x09
	.equ	IDE_LBA2, 0x0b
	.equ	IDE_HEAD, 0x0d
	.equ	IDE_CMD, 0x0f
	.equ	IDE_STATUS, 0x0f
	.equ	BSY, 7
	.equ	DRQ, 3
	.equ	ERR, 0

	.equ	SPT, 32			| logical sectors per track
	.equ	NFUNCS, 23

	.text
	.globl	_init
| ---------- cold start: called with JSR from the system ----------
_init:
	move.l	#entry,TRAP3
	lea	DUART,a0
	move.b	#0x00,IMR(a0)		| no interrupts until conin waits
	move.b	#RXVEC,IVR(a0)
	move.l	#rxint,RXVEC*4
	move.b	#0x10,CRA(a0)		| reset the MR pointer
	move.b	#0x13,MRA(a0)		| 8 bits, no parity
	move.b	#0x07,MRA(a0)		| 1 stop bit
	move.b	#0x80,ACR(a0)		| baud rate set 2
	move.b	#0xcc,CSRA(a0)		| 38400 baud
	move.b	#0x05,CRA(a0)		| enable receiver and transmitter
	move.b	#0x05,CRB(a0)
	move.l	#-1,buflba		| the sector buffer is empty
	clr.b	iobyte
	lea	signon,a1
1:	move.b	(a1)+,d1
	beq.s	2f
	bsr	conout
	bra.s	1b
2:	clr.l	d0			| drive A:, user 0
	rts

| ---------- TRAP #3: D0.W = function ----------
entry:
	cmp.w	#NFUNCS,d0
	bcc.s	1f
	movem.l	d1-d7/a0-a6,-(sp)
	and.l	#0xffff,d0
	lsl.w	#2,d0
	lea	functions,a0
	move.l	(a0,d0.w),a0
	jsr	(a0)
	movem.l	(sp)+,d1-d7/a0-a6
1:	rte

functions:
	.long	init, wboot, const, conin, conout, list, punch, reader
	.long	home, seldsk, settrk, setsec, setdma, read, write, listst
	.long	sectran, nop, getmrt, getiob, setiob, flush, setexc

init:	bra	_init
wboot:	movem.l	(sp)+,d1-d7/a0-a6	| drop the dispatcher's frame
	clr.l	d0
	jmp	CCP
nop:	rts

| ---------- character devices ----------
const:	clr.l	d0
	btst	#RXRDY,DUART+SRA
	beq.s	1f
	move.l	#0xff,d0
1:	rts

| Wait for a key without spinning: enable the receive interrupt and STOP
| until it comes (rxint masks it again, as the request stays asserted
| until the byte is read).
conin:	btst	#RXRDY,DUART+SRA
	bne.s	2f
	move.w	sr,-(sp)
1:	move.b	#RXINT_A,DUART+IMR
	stop	#0x2000
	btst	#RXRDY,DUART+SRA
	beq.s	1b
	move.w	(sp)+,sr
2:	clr.l	d0
	move.b	DUART+RHRA,d0
	rts

rxint:	move.b	#0,DUART+IMR
	rte

conout:	btst	#TXRDY,DUART+SRA
	beq.s	conout
	move.b	d1,DUART+THRA
	rts

list:
punch:	btst	#TXRDY,DUART+SRB
	beq.s	punch
	move.b	d1,DUART+THRB
	rts

reader:	btst	#RXRDY,DUART+SRB
	beq.s	1f
	clr.l	d0
	move.b	DUART+RHRB,d0
	rts
1:	move.l	#0x1a,d0		| no data: end of file
	rts

listst:	move.l	#0xff,d0
	rts

getiob:	clr.l	d0
	move.b	iobyte,d0
	rts
setiob:	move.b	d1,iobyte
	rts

| ---------- disk ----------
home:	clr.w	track
	rts

seldsk:	clr.l	d0
	tst.b	d1			| only A:
	bne.s	1f
	move.l	#dph,d0
1:	rts

settrk:	move.w	d1,track
	rts
setsec:	move.w	d1,sector
	rts
setdma:	move.l	d1,dma
	rts
sectran:
	move.l	d1,d0			| no skew
	rts

| D2 = IDE sector of (track, sector), A1 = its 128 bytes in the buffer.
locate:	clr.l	d2
	move.w	track,d2
	lsl.l	#3,d2			| 8 IDE sectors per track
	move.w	sector,d0
	move.w	d0,d1
	lsr.w	#2,d0
	add.l	d0,d2
	and.w	#3,d1
	lsl.w	#7,d1
	lea	buffer,a1
	add.w	d1,a1
	rts

| Make the buffer hold IDE sector D2. D0 = 0, or 1 on error.
fetch:	cmp.l	buflba,d2
	beq.s	9f
	move.l	#-1,buflba
	move.b	#0x20,d0		| READ SECTORS
	bsr	idecmd
	bne.s	8f
	lea	buffer,a0
	lea	IDE,a2
	move.w	#255,d1
1:	move.w	IDE_DATA(a2),d0		| the bus swaps the bytes of a word
	ror.w	#8,d0
	move.w	d0,(a0)+
	dbra	d1,1b
	move.l	d2,buflba
9:	clr.l	d0
	rts
8:	moveq	#1,d0
	rts

read:	bsr	locate
	bsr	fetch
	bne.s	9f
	move.l	dma,a0
	moveq	#127,d1
1:	move.b	(a1)+,(a0)+
	dbra	d1,1b
	clr.l	d0
9:	rts

write:	bsr	locate
	bsr	fetch
	bne.s	9f
	move.l	dma,a0
	moveq	#127,d1
1:	move.b	(a0)+,(a1)+
	dbra	d1,1b
	move.b	#0x30,d0		| WRITE SECTORS
	bsr	idecmd
	bne.s	8f
	lea	buffer,a0
	lea	IDE,a2
	move.w	#255,d1
2:	move.w	(a0)+,d0
	ror.w	#8,d0
	move.w	d0,IDE_DATA(a2)
	dbra	d1,2b
3:	btst	#BSY,IDE_STATUS(a2)
	bne.s	3b
	clr.l	d0
9:	rts
8:	move.l	#-1,buflba
	moveq	#1,d0
	rts

| Issue IDE command D0.B for one sector at LBA D2, then wait for data.
| Z set when ready, clear on error.
idecmd:	lea	IDE,a2
1:	btst	#BSY,IDE_STATUS(a2)
	bne.s	1b
	move.b	#1,IDE_COUNT(a2)
	move.b	d2,IDE_LBA0(a2)
	move.l	d2,d1
	lsr.l	#8,d1
	move.b	d1,IDE_LBA1(a2)
	lsr.l	#8,d1
	move.b	d1,IDE_LBA2(a2)
	lsr.l	#8,d1
	and.b	#0x0f,d1
	or.b	#0xe0,d1		| LBA, device 0
	move.b	d1,IDE_HEAD(a2)
	move.b	d0,IDE_CMD(a2)
2:	move.b	IDE_STATUS(a2),d1
	btst	#BSY,d1
	bne.s	2b
	btst	#ERR,d1
	bne.s	3f
	btst	#DRQ,d1
	beq.s	3f
	cmp.b	d1,d1			| Z = 1
	rts
3:	moveq	#1,d1			| Z = 0
	rts

flush:	clr.l	d0			| written through already
	rts

| ---------- memory and exceptions ----------
getmrt:	move.l	#memtab,d0
	rts

| D1.W = vector number, D2.L = new handler; D0 = the old one.
setexc:	and.l	#0xff,d1
	lsl.w	#2,d1
	move.l	d1,a0
	move.l	(a0),d0
	cmp.w	#TRAP3,d1		| keep the BIOS entry
	beq.s	1f
	move.l	d2,(a0)
1:	rts

	.data
signon:	.ascii	"\r\nrs6809 Tiny68K BIOS for CP/M-68K 1.3\r\n\0"
	.even
memtab:	.word	1
	.long	TPA_BASE, TPA_END-TPA_BASE

| Disk parameter header and block of drive A:.
dph:	.long	0			| no sector translation
	.word	0, 0, 0			| BDOS scratch
	.long	dirbuf, dpb, csv, alv
dpb:	.word	SPT			| SPT
	.byte	5, 31			| BSH, BLM: 4 KiB blocks
	.byte	1, 0			| EXM, (unused)
	.word	2046			| DSM: 2047 blocks
	.word	511			| DRM: 512 entries
	.byte	0xf0, 0			| AL0, AL1: 4 directory blocks
	.word	0			| CKS: fixed disk
	.word	1			| OFF: reserved tracks

	.bss
track:	.space	2
sector:	.space	2
dma:	.space	4
buflba:	.space	4
iobyte:	.space	2
dirbuf:	.space	128
csv:	.space	2
alv:	.space	258
	.even
buffer:	.space	512
	.end
