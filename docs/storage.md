# QEMU FAT32 storage

Storage is optional on AArch64 QEMU `virt`. The readonly initramfs remains `/`;
the first discovered disk and its first primary MBR FAT32 partition provide
optional boot mount configuration. Without `/fstab` on that partition, it mounts
at `/mnt/disk`. A missing disk is normal. Invalid selected storage or present
but invalid configuration fails initialization explicitly. Raspberry Pi SDHOST
is not implemented.

## Boot mount configuration

On the current QEMU backend, the first ordinary initialization task discovers
block devices, reads the first disk's MBR, and opens its first type 0x0b/0x0c
partition in MBR slot order. It
looks for `/fstab` at the root of that FAT32 filesystem, not in the initramfs.
It does not search another partition or disk when the selected one is invalid
or has no FAT32 partition.

Configuration-source selection is distinct from the partitions named in fstab:

- QEMU loads the kernel through `-kernel`, not from an attached disk. The first
  discovered disk therefore supplies configuration by convention.
- For hardware boot from storage, the requested policy is to use the medium
  from which the kernel was loaded, then its first FAT32 partition. On a Pi
  booted from microSD, this means the SD card, not an unrelated first-probed disk.
  This branch is not implemented: genrt has neither a Pi SD driver nor a
  loader-to-driver boot-medium identity contract yet. See the
  [storage bootstrap backlog](roadmap/hardening.md#storage-bootstrap).

Boot-medium identity must come from explicit platform/loader information, not
the FIT's RAM address or an assumption about device discovery order. The fstab
entries may still select partitions on other discovered disks.

- If `/fstab` is absent, mount that partition at `/mnt/disk`.
- If `/fstab` exists, mount its automatic entries in file order. There is no
  implicit `/mnt/disk` mount; an empty configuration mounts no disk partitions.
- Read errors, a directory named `fstab`, unsupported fields, and invalid mount
  targets are errors, not reasons to silently use the fallback.

The supported six-field format is:

```fstab
# source          mountpoint       type   options   dump pass
/dev/disk0p1      /mnt/disk        vfat   defaults  0    0
/dev/disk0p2      /mnt/disk/DATA   fat32  ro        0    0
```

`disk0` is the first successfully probed block device in firmware discovery
order. Partition numbers are original primary MBR slots 1 through 4, including
holes; they are not FAT-only ordinals. These strings identify boot sources and
do not imply that a devfs or userspace device nodes exist.

Only FAT32 (`vfat` or `fat32`) is supported. Options are `defaults`, `rw`, `ro`,
and `noauto`; `defaults` uses read-write access subject to physical write
protection. `noauto` skips automatic mounting. Both final fields must be zero:
boot-time dump and fsck are not implemented. The file is limited to 16 KiB and
16 entries. Duplicate sources are rejected, preventing
two independent FAT caches from mutating the same partition.
Blank lines and comments are accepted; octal escapes such as `\040` encode
spaces within fields. Up to 32 discovered disks can be staged for boot.

Mountpoints must already be directories. In the example, `DATA` must exist on
the first partition before mounting the second. Parent mounts precede nested
mounts. Root replacement and dynamic unmount remain unsupported. Discovery and
configuration reading reuse the first filesystem instance when it is mounted;
they do not open a second independently cached instance of that partition.

## Create a data disk

Run from the repository root:

```bash
cargo xtask disk-create --output target/data.raw --size-mib 128 --label GENRT
cargo xtask run-aarch64 --cpus 4 --disk target/data.raw
```

`disk-create` needs `sfdisk` from util-linux (the `fdisk` package on Ubuntu),
`mkfs.fat` from dosfstools, and GNU `timeout` from coreutils, all included by
the host setup scripts. Populating partitions also needs `mmd` and `mcopy` from
mtools. It requires no root privileges,
loop device, or host mount. The output's parent directory must already exist
and support hard links. The command refuses any existing output entry,
including symlinks and device nodes; there is no overwrite/force option.

`--size-mib` is the whole disk size, defaulting to 128 MiB, with a minimum of
65 MiB. By default, `sfdisk` creates one type 0x0c FAT32 partition using its
default alignment and available space; xtask does not prescribe a start sector.
The formatter uses the resulting table's extent, which is also printed after
creation. Disk sizes must fit the MBR sector range.
`--label` defaults to `GENRT` and accepts at most 11 printable ASCII bytes,
subject to FAT label restrictions. Formatting uses 512-byte sectors and
`mkfs.fat --invariant`; cluster size is selected automatically by `mkfs.fat`.
The MBR disk ID is fixed for reproducibility.

Formatting happens in a private sibling directory. Only a successfully
formatted and synced image is published, without replacing an existing path.
Normal failures remove the unpublished work; abrupt termination can leave a
`.genrt-disk-*` staging directory. Each partitioning, formatting or copy command is
limited to 60 seconds with a further two-second forced-termination grace period.

Without `--layout`, the filesystem is empty: no kernel, initramfs, bootloader,
fstab, or test files are installed. QEMU loads the kernel separately; without
fstab genrt mounts the new filesystem at `/mnt/disk`. This is a data image,
not bootable Pi media.
Large FAT volumes retain larger caches in genrt; successful formatting alone
does not establish that an arbitrary size fits the configured kernel heap.

With QEMU stopped, inspect or populate the partition using mtools. A temporary
drive definition selects primary partition 1 directly from the MBR, without
hardcoding its byte offset:

```bash
mtools_config=$(mktemp)
printf 'drive g: file="%s" partition=1\n' "$(realpath target/data.raw)" > "$mtools_config"
MTOOLSRC="$mtools_config" mdir g:/
MTOOLSRC="$mtools_config" mcopy ./my-file.txt g:/
# Optional: copy an existing six-field configuration to the partition root.
MTOOLSRC="$mtools_config" mcopy ./fstab g:/fstab
rm -- "$mtools_config"
```

Never modify or format an image concurrently with a running guest. Reuse the
same `--disk` path for later launches to retain files; do not rerun disk-create.

### Multiple partitions and initial files

The repository includes a ready-to-run [disk.toml](../examples/storage/disk.toml) and source trees
under `examples/storage/`. From the repository root:

```bash
cargo xtask disk-create --output target/data.raw --size-mib 256 \
  --layout examples/storage/disk.toml
cargo xtask run-aarch64 --cpus 4 --disk target/data.raw
```

Its first partition supplies fstab and mounts at `/mnt/disk`; the second mounts
readonly at `/mnt/disk/DATA`. In the guest, `cat /mnt/disk/DATA/hello.txt` reads
the sample file. A `.gitkeep` retains the first partition's mountpoint directory
in Git; that directory is covered by the second mount. Use a new output path
if `target/data.raw` already exists.

Use `--layout <disk.toml>` to declare up to four primary FAT32 partitions and
optional source directories. The supplied example has this structure:

```text
examples/storage/
  disk.toml
  boot/
    fstab
    DATA/.gitkeep
  data/
    hello.txt
```

The corresponding `disk.toml` is:

```toml
schema = 1

[[partitions]]
size_mib = 64
label = "BOOT"
source = "boot"

[[partitions]]
label = "DATA"
source = "data"
```

Each partition requires a label. `size_mib` is the partition size, not its start;
it must be at least 64 MiB. Only the last partition may omit it, consuming the
remaining aligned space. The total `--size-mib` must accommodate every partition
and the partition table/alignment space. If all sizes are explicit, remaining
space stays unused. Alignment and cluster sizing remain automatic.

`source` is optional; absence creates an empty filesystem. Relative sources
resolve against the TOML file's directory, not the current working directory.
Absolute paths and `../` references may explicitly select other source roots.
The directory's contents go directly into the partition root. Files, dotfiles
and empty directories are included in sorted order; symlinks within that tree,
special files, invalid FAT names and case-conflicting names fail creation.
Source trees must remain unchanged while building. Copied entry timestamps
are fixed at 1980-01-01 UTC; host timestamps and permissions are not preserved
or modified. No fstab is generated or rewritten: place the desired file in
the first source tree.

The supplied `examples/storage/boot/fstab` contains:

```fstab
/dev/disk0p1 /mnt/disk      vfat defaults 0 0
/dev/disk0p2 /mnt/disk/DATA vfat ro       0 0
```

`--label` applies only to the default single-partition mode and cannot accompany
`--layout`. Both modes refuse existing output paths. Every partition is formatted
and populated before publication, so a failed later copy never publishes a
partially prepared image. With this fstab, `hello.txt` appears at
`/mnt/disk/DATA/hello.txt`; `DATA` must exist on the first partition before boot.

## Launch an existing image

Use an owned, sector-aligned regular raw image with an MBR partition of type
0x0b or 0x0c and a 512-byte-sector FAT32 volume. Stop every other VM/tool using
that file before attaching it read-write:

```bash
cargo xtask run-aarch64 --cpus 4 --disk /absolute/path/disk.raw
cargo xtask build-aarch64 --cpus 4 --disk /absolute/path/disk.raw
cargo xtask qemu-cmd --arch aarch64 --cpus 4 --disk /absolute/path/disk.raw
```

`debug-aarch64` accepts the same option. `qemu-cmd` previews the debugging command
for already-built artifacts. Disk and diskless DTBs have different filenames;
both DTB dump and launch use the same modern virtio-mmio attachment. Filenames
are encoded as JSON blockdev arguments, not interpolated into QEMU option CSV.
These attachment commands never create, format or repartition user images;
only the explicit `disk-create` command provisions a new one. Without `--disk`, normal
run/debug/release composition remains diskless. The standalone release launcher
does not attach disks.

The shell can inspect mounted files using `ls /mnt/disk` and `cat`; it does not
implement redirection. Freestanding C callers use ordinary `open` with
`O_CREAT`, `O_RDWR`/`O_WRONLY`, `O_TRUNC`, or `O_APPEND`, then read/write and
`fsync(fd)` before closing. File I/O has no fixed 1024-byte cap. Handle short
transfers and negative errors; large OFD staging grows fallibly and is reused.
ELF execution still requires archive-backed ramfs, not FAT files.

## Reproducible verification

Host setup installs `dosfstools`, `mtools`, and GNU coreutils (`timeout`). Run:

```bash
cargo test -p kernel --lib --locked
cargo test -p xtask --locked
cargo xtask test-aarch64 --case storage-contract
cargo xtask test-aarch64 --case storage-contract --profile release \
  --artifacts-dir target/test-results-storage-release
```

The storage contract separately recreates its owned `storage-test.raw`: a 129 MiB image
with two 64 MiB FAT32 partitions starting at sectors 2048 and 133120, with
host-seeded content. Never point
the fixture builder at a valuable disk. It is not a general provisioning API.
Four guest CPUs exercise traversal, long names, writable/multi-cluster files,
truncate/append coherence, fork-shared offsets and concurrent append.

The first QEMU process has no `/fstab`: it checks fallback placement, writes
65573 deterministic bytes to `PERSIST.BIN`, fsyncs and powers off. After reaping
it, xtask extracts the file with `mcopy`, compares all bytes and SHA-256, and runs
`fsck.fat -n` on copies of both partitions. The host then adds `/fstab` to the
first partition. A second QEMU process boots the same raw image with a distinct
verify-only initramfs input; it must read the existing payload and mount the
second partition readonly at `/mnt/disk/EXTRA`. It checks readonly enforcement,
mount-parent traversal and cross-CPU operations on the two volumes sharing one
controller. Host checks repeat after the second poweroff.
Evidence lives below `target/test-results/storage-contract/`: both serial logs,
JSON results, raw image, extracted payload and `persistence.sha256`.

With QEMU stopped, manual host inspection of that controlled image is:

```bash
mdir -i target/test-results/storage-contract/storage-test.raw@@1048576 ::/
```

The [library PoC](../tests/storage/fat-poc/README.md) compares resident-fat and
starry-fatfs independently of QEMU. Its persistent-handle follow-up measures
canonical handle reuse and records starry lifetime/Send and metadata-sync
limitations. The production adapter now uses resident-fat under ADR-0067, not
the experimental starry handles. Host mocks test real transport/queue code,
DMA lifetime, discovery, MBR bounds and failures. FAT tests exercise retained
handles, small library allocations and injected read/write/sync faults.
These are functional/ownership checks, not RT or
power-loss guarantees.

## Ownership and limits

- The common `kernel::dtb` reader validates runtime framing once and decodes
  enabled nodes and physical MMIO resources, including ancestor `ranges`.
  AArch64 `platform/bootstrap` performs late device matching after runtime
  VM/heap activation. It resolves the already selected GIC domain once, maps
  MMIO and registers the probed device. Concrete controller modules
  live in `platform/block`: `virtio_block` identifies the mapped controller and
  owns its transport, queue, DMA and request lifecycle. Drivers receive mapped
  resources, not DTB nodes. Raspberry Pi SD support is not implemented.
- Generic block I/O uses exclusive owners and checked partition-relative
  addressing. The QEMU backend uses DTB resources and a preallocated split
  virtqueue/request page. Caller buffers never enter DMA.
- Partitions of one physical disk share a logical device reservation. A FAT
  operation reserves the physical owner before library work; contention returns
  `Pending` with an exact wait token. Sector calls use the reserved owner without
  holding a spinlock or entering a nested scheduler wait. Different partitions
  retain separate filesystem caches; one partition is mounted only once.
- The first initialization thread constructs and publishes the namespace before
  spawning EL0. Runtime FAT calls run with IRQs enabled, outside process,
  scheduler, OFD and volume spinlocks. Competing volume users publish exact
  waits and resume through existing syscall restart, without resubmitting work.
- `resident-fat` 0.3.1 retains the FAT and parsed directories. Each stable node
  owns a directory cluster or one canonical library file. All OFDs use that
  same current file metadata, with separate VFS positions. Payload I/O does
  not remount or reopen by path. Append holds the logical lease across all
  fragments. The private transferable owner permits only the default epoch
  clock; no borrowed handle lifetime is extended.
- Small library cache/run metadata allocations are permitted in task context;
  generic OFD-owned staging still has a reusable 4096-byte baseline. Resident
  cache memory scales with volume geometry and traversed directories, not
  merely with open descriptor count. This is not an allocation-free FAT API.
- Iterative directory resolution includes the 32-component path checked by
  QEMU. Ordinary BPB validation and device-capacity checks belong to the
  library. The adapter preflight only guards unchecked FAT-region arithmetic,
  validates the root and concrete FSInfo location, and rejects unsupported
  active-FAT selection or FAT32 revision. FSInfo `0`/`0xffff` means absent;
  the unused backup-boot-sector field does not restrict mounting. No duplicate
  cluster-count threshold or FAT-bit-size ceiling is imposed by the adapter.
- `fsync` publishes FAT/FSInfo and library dirty-state metadata, then performs
  transport FLUSH without unmounting. Close/drop perform no disk I/O. Ordinary
  writes do not imply durability; explicitly sync before powering off.
- The library does not return partial progress on error. Sector-fragment calls
  preserve only confirmed successful prefixes. A failed fragment may have
  changed data before a metadata failure; it is not counted or retried.
  Device and inconsistent-metadata errors latch so later calls cannot
  conceal failure or claim a successful sync.
- Block calls permit 128 sectors, one outstanding request, one-second request
  and five-second total-transfer budgets. FAT has no additional operation-wide
  call counter or deadline; valid large/slow operations do not poison a volume
  merely by taking more steps or time. CPU-only chain scans and directory
  parsing have no hard wall-time bound; corruption checks belong to the FAT
  library. Real device timeouts still latch and prevent further submissions.
- FAT supports 4096 discovered nodes, 1024-byte local paths, 32 components,
  and the VFS 255-byte component limit. Mirroring-disabled/active-FAT volumes
  are rejected with `ENOTSUP`: the pinned library always reads the first FAT
  and updates every copy. Legacy non-ASCII OEM short names without
  an LFN are unsupported; UTF-8 long names use the library's Unicode handling.
  No GPT/extended partitions, dynamic unmount, page cache, permissions, links,
  journal or crash-consistency recovery is added.

Storage operations are outside RT benchmark measurement. IRQ/scheduler/timer
paths allocate nothing and do not wait on the disk. Polling is confined to
bounded ordinary-thread driver operations; the parsed device SPI stays masked.
An uncertain DMA completion poisons the device and retains its DMA backing.
Do not infer physical hardware durability from QEMU FLUSH or emulator timing.

See [ADR-0064](../memory/decisions/ADR-0064-qemu-block-storage-and-fat32.md),
[resident handle ownership](../memory/decisions/ADR-0067-resident-fat-and-canonical-file-handles.md),
[driver-owned timeouts](../memory/decisions/ADR-0068-driver-owned-storage-timeouts.md),
[shared DTB and staged probe](../memory/decisions/ADR-0069-shared-runtime-dtb-and-staged-probe.md),
[boot mount policy](../memory/decisions/ADR-0070-fstab-boot-mounts-and-shared-disks.md),
[fsync ABI](../memory/decisions/ADR-0065-fsync-syscall.md), and
[usercopy lengths](../memory/decisions/ADR-0066-usercopy-length-and-owned-io-growth.md).
