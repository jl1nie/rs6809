* Small OS-9 Level 1 native C driver. MIT, like rs6809.
* Usage: cc basename[.c] (current data directory, simple filenames).
                    nam cc
Level               equ 1
                    use os9.d
tylg                set Prgrm+Objct
atrv                set ReEnt+1
                    mod eom,name,tylg,atrv,start,size
                    org 0
base                rmb 2
stage               rmb 2
program             rmb 2
paramlen            rmb 2
savedout            rmb 1
status              rmb 1
redirect            rmb 1
stem                rmb 32
params              rmb 192
path                rmb 64
                    rmb 512
size                equ .
name                fcs /cc/
                    fcb 1
start               stu <base
                    leay stem,u
                    ldb #31
skipspace           lda ,x+
                    cmpa #$20
                    beq skipspace
parse               cmpa #$0d
                    beq parsed
                    cmpa #$20
                    beq parsed
                    cmpa #'.
                    beq extension
                    cmpa #'/
                    lbeq usage
                    cmpa #$21
                    lblo usage
                    cmpa #$7e
                    lbhi usage
                    stb <status
                    sta ,y+
                    decb
                    lbeq usage
                    lda ,x+
                    bra parse
extension           lda ,x+
                    anda #$df
                    cmpa #'C
                    lbne usage
                    lda ,x+
                    cmpa #$0d
                    beq parsed
                    cmpa #$20
                    lbne usage
parsed              cmpb #31
                    lbeq usage
                    lda #0
                    sta ,y
                    leax stages,pcr
                    stx <stage
nextstage           ldx <stage
                    ldd ,x
                    lbeq success
                    leax d,x
                    stx <program
                    lda #2
                    ldy #80
                    os9 I$WritLn
                    ldx <stage
                    lda 4,x
                    sta <redirect
                    leax 2,x
                    ldd ,x
                    leax d,x
                    lbsr expand
                    std <paramlen
                    tst <redirect
                    beq execute
* Prepare stdout redirection for c.prep; retain terminal on a spare path.
                    lda #1
                    os9 I$Dup
                    lbcs failed
                    sta <savedout
                    ldx <base
                    leay path,x
                    leax outpath,pcr
                    lbsr expandto
                    ldx <base
                    leax path,x
                    os9 I$Delete
                    lda #1
                    os9 I$Close
                    lbcs failed
                    ldx <base
                    leax path,x
                    lda #2
                    ldb #$0b
                    os9 I$Create
                    lbcs restorefailed
execute             ldx <program
                    ldy <paramlen
                    ldu <base
                    leau params,u
                    lda #Prgrm+Objct
                    clrb
                    os9 F$Fork
                    bcs childfailed
                    os9 F$Wait
                    bcs childfailed
                    bra result
childfailed         orcc #Carry
result              stb <status
                    tst <redirect
                    beq checkstatus
                    lda #1
                    os9 I$Close
                    lda <savedout
                    os9 I$Dup
                    bcs failed
                    lda <savedout
                    os9 I$Close
                    bcs failed
checkstatus         ldb <status
                    bne failed
                    ldx <stage
                    leax 5,x
                    stx <stage
                    lbra nextstage
success             clrb
                    os9 F$Exit
restorefailed       stb <status
                    lda <savedout
                    os9 I$Dup
                    lda <savedout
                    os9 I$Close
                    ldb <status
failed              pshs b
                    leax failure,pcr
                    lda #2
                    ldy #80
                    os9 I$WritLn
                    puls b
                    os9 F$Exit
usage               leax help,pcr
                    lda #2
                    ldy #80
                    os9 I$WritLn
                    ldb #E$BPNam
                    os9 F$Exit
* Expand '%' into stem; templates include a terminating CR.
* D returns the expanded length, including CR.
expand              ldu <base
                    leay params,u
expandto            pshs u,y
expandloop          lda ,x+
                    cmpa #'%
                    bne literal
                    ldu <base
                    leau stem,u
copyname            lda ,u+
                    beq expandloop
                    sta ,y+
                    bra copyname
literal             sta ,y+
                    cmpa #$0d
                    bne expandloop
                    tfr y,d
                    subd ,s
                    puls pc,u,y
stages              fdb prep-*
                    fdb prepparams-*
                    fcb 1
                    fdb pass1-*
                    fdb pass1params-*
                    fcb 0
                    fdb pass2-*
                    fdb pass2params-*
                    fcb 0
                    fdb opt-*
                    fdb optparams-*
                    fcb 0
                    fdb asm-*
                    fdb asmparams-*
                    fcb 0
                    fdb link-*
                    fdb linkparams-*
                    fcb 0
                    fdb 0
prep                fcc /c.prep/
                    fcb $0d
pass1               fcc /c.pass1/
                    fcb $0d
pass2               fcc /c.pass2/
                    fcb $0d
opt                 fcc /c.opt/
                    fcb $0d
asm                 fcc /c.asm/
                    fcb $0d
link                fcc /c.link/
                    fcb $0d
prepparams          fcc /%.c/
                    fcb $0d
outpath             fcc /%.m/
                    fcb $0d
pass1params         fcc /%.m -o=%.i/
                    fcb $0d
pass2params         fcc /%.i -o=%.a/
                    fcb $0d
optparams           fcc /%.a %.o/
                    fcb $0d
asmparams           fcc /%.o -o=%.r/
                    fcb $0d
linkparams          fcc |/dd/lib/cstart.r %.r -o=% -l=/dd/lib/clib.l|
                    fcb $0d
help                fcc /Usage: cc basename[.c]/
                    fcb $0d
failure             fcc /cc: compilation failed/
                    fcb $0d
                    emod
eom                 equ *
                    end
