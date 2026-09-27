ENTRY(_start)

SECTIONS
{
    . = 0x400000;

    .text : { *(.text .text.*) }
    .rodata : { *(.rodata .rodata.*) }

    /* Start the writable segment on a fresh page. The kernel loader maps a
       page once with the first segment's protection, so a read-only/writable
       page shared with `.rodata` would stick read-only and the first write
       (the heap bookkeeping in `.bss`) would fault. */
    . = ALIGN(0x1000);
    .data : { *(.data .data.*) }
    .bss : { *(.bss .bss.*) *(COMMON) }

    /DISCARD/ : { *(.eh_frame) *(.eh_frame_hdr) *(.comment) }
}
