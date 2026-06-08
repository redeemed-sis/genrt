ENTRY(_start)

USER_TEXT_BASE = 0x0000004000000000;

PHDRS
{
  text PT_LOAD FLAGS(5);
  data PT_LOAD FLAGS(6);
}

SECTIONS
{
  . = USER_TEXT_BASE;

  .text : ALIGN(0x1000)
  {
    KEEP(*(.text.start))
    *(.text .text.*)
  } :text

  .rodata : ALIGN(0x1000)
  {
    *(.rodata .rodata.*)
  } :text

  . = ALIGN(0x1000);

  .data : ALIGN(0x1000)
  {
    *(.data .data.*)
  } :data

  .bss (NOLOAD) : ALIGN(0x1000)
  {
    *(.bss .bss.*)
    *(COMMON)
  } :data

  /DISCARD/ :
  {
    *(.comment)
    *(.note .note.*)
    *(.eh_frame)
  }
}
