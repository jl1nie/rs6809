* F$CpyMem for the legacy Multicomp 6809 kernel. The bundled KrnP2
* expects this service in Krn, but mc09krn only supplies it for 6309.
* Use published services here rather than Krn's private entry points.
FCpyMem             ldd       R$Y,u
                    beq       CpyDone
                    addd      R$U,u
                    cmpa      #$FE
                    bhs       CpyDone
                    leas      -16,s              temporary DAT image
                    leay      ,s
                    pshs      y,u
                    ldx       <D.Proc
                    lda       #8
                    ldb       P$Task,x
                    pshs      d                  counter, caller task
                    leay      P$DATImg,x
                    ldx       R$D,u              DAT pointer in caller space
                    ldu       2,s
CpyImage            clra
                    clrb
                    os9       F$LDDDXY
                    std       ,u++
                    leax      2,x
                    dec       ,s
                    bne       CpyImage
                    os9       F$ResTsk
                    bcs       CpyClean
                    stb       ,s                 temporary task number
                    lslb
                    ldx       <D.TskIPt
                    ldu       2,s
                    stu       b,x
                    ldu       4,s                original register frame
                    ldd       ,s+                temp task, caller task; eat one byte
                    sta       ,s                 save temp task for release
                    ldx       R$X,u
                    ldy       R$Y,u
                    ldu       R$U,u
                    os9       F$Move
                    puls      b
                    os9       F$RelTsk
                    leas      -2,s
CpyClean            leas      22,s
                    rts
CpyDone             clrb
                    rts
