/* C side of the thumbv7m board (ADR 0077): the semihosting request that
 * stops QEMU. Erebos has no inline assembly before M4. */

/* SYS_EXIT_EXTENDED (0x20) with the reason ADP_Stopped_ApplicationExit
 * (0x20026) and `code`, which QEMU, started with
 * `-semihosting-config enable=on`, turns into its exit status. */
void board_exit(unsigned code) {
    unsigned block[2] = { 0x20026u, code };
    register unsigned request __asm__("r0") = 0x20u;
    register unsigned *argument __asm__("r1") = block;
    __asm__ volatile("bkpt 0xab" : "+r"(request) : "r"(argument) : "memory");
}
