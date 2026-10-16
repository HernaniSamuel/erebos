/* C side of startup_bss.ers: runs the startup code again, which Erebos
 * cannot name (ADR 0012). */
void _start(void);

void c_restart(void) { _start(); }
