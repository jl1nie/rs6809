/* Offsets the FUZIX monitor (src/ice/fuzix.rs, mod off) reads, from the
   kernel headers for Tiny68K. scripts/build-fuzix-tiny68k.sh compiles this
   to assembly and writes the .equ values to artifacts/fuzix68k/offsets.txt;
   tests/fuzix68k.rs compares them with the monitor. */
#include <kernel.h>
#include <stddef.h>
#define O(n, v) __asm__ volatile (".equ " #n ", %c0" :: "i" (v))
void f(void) {
	O(PTAB_SIZE, sizeof(struct p_tab));
	O(P_STATUS, offsetof(struct p_tab, p_status));
	O(P_TTY, offsetof(struct p_tab, p_tty));
	O(P_PID, offsetof(struct p_tab, p_pid));
	O(P_UID, offsetof(struct p_tab, p_uid));
	O(P_PPTR, offsetof(struct p_tab, p_pptr));
	O(P_EXITVAL, offsetof(struct p_tab, p_exitval));
	O(P_WAIT, offsetof(struct p_tab, p_wait));
	O(P_UDATA, offsetof(struct p_tab, p_udata));
	O(P_PRIORITY, offsetof(struct p_tab, p_priority));
	O(P_NAME, offsetof(struct p_tab, p_name));
	O(P_TIME, offsetof(struct p_tab, p_time));
	O(P_PGRP, offsetof(struct p_tab, p_pgrp));
	O(P_NICE, offsetof(struct p_tab, p_nice));
	O(P_SIZE, offsetof(struct p_tab, p_size));
	O(U_PTAB, offsetof(struct u_data, u_ptab));
	O(U_INSYS, offsetof(struct u_data, u_insys));
	O(U_CALLNO, offsetof(struct u_data, u_callno));
	O(U_NAME, offsetof(struct u_data, u_name));
	O(U_CODEBASE, offsetof(struct u_data, u_codebase));
	O(U_TOP, offsetof(struct u_data, u_top));
}
