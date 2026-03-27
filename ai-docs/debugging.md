# Debugging

## AArch64 QEMU + GDB

Start QEMU in paused mode with a GDB stub:

```bash
just qemu-cmd-aarch64
```

Then in another terminal:

```bash
just gdb-cmd-aarch64
```

For Week 1 this is scaffolding only.
In Week 2 the command will point to a real bootable kernel ELF.
