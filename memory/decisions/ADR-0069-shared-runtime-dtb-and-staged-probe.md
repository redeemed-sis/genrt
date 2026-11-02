# ADR-0069: Shared runtime DTB and staged driver probing

## Status

Accepted

Extends boot-memory, platform and storage initialization ownership; no previous
decision is superseded. Pre-MMU boot protocol and syscall ABI are unchanged.

## Context

Memory, CPU/PSCI, Pi UART, chosen payloads and storage constructed independent
runtime FDT readers. Virtio discovery also carried a structural validator and
searched for an interrupt controller for every transport. Driver-local firmware
decoding duplicated bootstrap responsibilities.

## Decision

- `kernel::dtb::RuntimeDtb` borrows immutable firmware bytes and wraps the
  existing fdt-raw parser. Bootstrap publishes one permanent view, without
  allocating. Consumers reuse it rather than reconstructing raw pointers or
  repeating header/framing validation.
- The common reader checks structural/string/reservation-map framing within
  parser-supported depth/path limits. It provides enabled-node iteration and
  checked physical MMIO extents through ancestor `ranges`. CPU `reg` values
  without sizes and domain-specific properties remain available through the
  same raw reader. This is not exhaustive validation of every DT binding.
  Ancestor translation retains compact borrowed bus metadata; it does not nest
  dependency parser lookups on the bounded kernel stack.
- Status filtering is explicit. Pi UART adoption includes disabled serial
  nodes because firmware already selected the transmitter. Driver code receives
  decoded register ranges and does not parse DTB bytes.
- AArch64 bootstrap has an allocation-free early stage for UART, CPU/firmware
  metadata, initrd handoff, generic boot information and global IRQ setup.
  The independent `.boot.*` parser still extracts only pre-MMU mapping inputs.
- Late optional-device discovery runs from the ordinary init task after runtime
  VM/heap activation, with IRQs enabled and no shared lock held across probing.
  Separate late DTB traversal is allowed. It resolves the existing GIC's domain
  once by its authoritative MMIO windows; each device's interrupt parent must
  match. Interrupt encodings and masked-SPI register checks remain AArch64-owned.
- Bootstrap selects and connects concrete drivers. Drivers own transport,
  DMA and request lifetime; no generic dynamic registry or heap-built device
  graph is introduced. Pi SD support remains outside this change.

## Invariants

Firmware backing stays reserved and mapped across runtime TTBR1 replacement.
No parsing, allocation or device waits enter IRQ/scheduler/timer dispatch.
No new pre-MMU dependencies are introduced. Initrd publication still precedes
memory initialization, and late block probing still precedes namespace publish.

## Consequences

One constructor owns runtime framing checks. Resource consumers may traverse
the borrowed tree more than once without duplicating parser setup or policy.
Invalid framing fails earlier; optional storage semantics are still checked
only during late discovery. Supported bus decoding remains bounded, not a
general device model, interrupt nexus implementation or overlay engine.

## Alternatives considered

A new parser crate or owned device graph adds packaging/lifecycle complexity
without a present requirement. Merely moving files leaves duplicated runtime
construction and controller discovery. Removing all resource validation would
discard the MMIO and DMA ownership prerequisites needed by drivers.

## Validation

Reuse existing kernel, discovery/queue and QEMU contracts. The existing Pi
backend host check also decodes the bundled board DTB through the common
reader, including its disabled PL011 and mixed-width bus translation. Run
diskless CI, debug/release storage persistence and Pi FIT builds. These checks
do not establish physical Pi boot or storage support. Inspect linked debug
parser call chains for stack capacity and interrupt headroom; a passing guest
contract alone does not detect every write below an unguarded stack.

## Related decisions

- [ADR-0007](ADR-0007-dtb-memory-map-and-frame-allocator.md)
- [ADR-0051](ADR-0051-raspberrypi-runtime-spin-table-and-power.md)
- [ADR-0064](ADR-0064-qemu-block-storage-and-fat32.md)
- [AArch64 boot](../../arch/aarch64/README.md)
- [Storage guide](../../docs/storage.md)
