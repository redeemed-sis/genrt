# Block ownership

`BlockDevice` is an exclusive `Send` owner, not a shared concurrent handle.
Capacity and request addresses are 512-byte sectors; transfers must be aligned,
in range and at most 128 sectors. Empty in-range I/O is a no-op. Driver errors
can follow a changed prefix: multi-sector operations are not atomic.

`Mbr` validates the signature, flags, all four primary extents and overlap.
It selects the first FAT32 type 0x0b/0x0c in table order. `PartitionDevice`
revalidates even manually constructed extents and translates each request
without exposing sectors outside the partition. GPT/extended traversal is not
implemented.

A bounded staging registry moves discovered device owners into the first
initialization thread in firmware order. On the current QEMU backend the first
disk supplies boot mount configuration; it is not implicitly the only usable
disk or evidence of the medium used to load the kernel. Partitions sharing a
controller reserve one physical owner before FAT work and return it afterward.
Contention registers exact wait tokens; short locks only move ownership and
registrations. All allocation, sector I/O, flush and destruction occur outside
those locks. Drivers must bound requests and whole transfers and never retain
caller buffers after return. The current QEMU architecture backend preallocates DMA
storage and poisons/leaks uncertain device-owned resources after timeout.

See [storage](../../../docs/storage.md) for the FAT adapter, ordinary-thread
execution policy, canonical commands and actual verification procedure.
