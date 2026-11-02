# FAT library comparison

This isolated host/no-std PoC executes resident-fat 0.3.1 and starry-fatfs
0.4.1-preview.2 on independent copies of the same 64 MiB FAT32 seed. It is not
linked into production or included in routine CI. Both dependencies are pinned
in its separate lockfile. `src/lib.rs` supplies no-std sector/byte adapters;
`src/main.rs` supplies memory transport, fault injection, allocation counters,
independent-open models and persistent-image assertions.

Run from the repository root using a fresh owned temporary directory:

```bash
work=$(mktemp -d /tmp/genrt-fat-poc.XXXXXX)
truncate -s 64M "$work/seed.img"
mkfs.fat --invariant -F 32 -S 512 -s 1 -i 00240024 -n GENRTPOC "$work/seed.img"
cargo run --manifest-path tests/storage/fat-poc/Cargo.toml --locked --release -- \
  "$work/seed.img" "$work/resident.img" "$work/starry.img"
cargo build --manifest-path tests/storage/fat-poc/Cargo.toml --locked --release \
  --lib --target aarch64-unknown-none-softfloat
fsck.fat -vn "$work/resident.img"
fsck.fat -vn "$work/starry.img"
fsck.fat -vn "$work/short-lived-starry.img"
```

The harness creates the short-lived image beside `starry.img`. Seed and outputs
are raw volumes, not MBR disks. Never supply a valuable image as an output.
An optional `--features compare-alloc` compares starry's heap-backed LFN mode;
the original allocation-free starry comparison keeps that feature disabled.

Measured default-mode results: resident mount retains 517105 heap bytes, small
overwrites allocate twice each, and a 2 MiB incremental write allocates 12311
times. Starry performs those I/O calls and the complete LFN
mount/open/I/O/flush/unmount cycle without allocations, frees or reallocations.
Both stale-writer models lose length (2048 becomes 1025); fresh reconstruction
under one volume owner avoids that trap. Starry exposes partial write progress;
resident may return an error after changing an unreported prefix.

The harness also covers append/truncate, independent reopen, cross-library
reads, disk-full, invalid geometry/signature, injected reads/writes/sync failures,
and explicit unmount/outer-flush behavior. Counts exclude preallocated host image
storage. This is feasibility evidence, not kernel/SMP/DMA, power-loss or target
latency validation. See [ADR-0064](../../../memory/decisions/ADR-0064-qemu-block-storage-and-fat32.md).

## Persistent-handle feasibility

`src/bin/retained-handles.rs` keeps one filesystem and directory alive, pairs
a node ID with one authoritative library file, and models two independent OFD
positions. It uses only public library APIs, with no lifetime extension, leaked
owners or unsafe Send/Sync implementations. The filesystem owner remains a
separate local variable; this does not solve kernel owner/dependent storage.
The original comparison remains the package's default binary.

| Check | Observed result |
| --- | --- |
| Two independently retained library files | Sequential writes reduce length from 2048 to 1025 |
| One canonical file, two OFD positions | Overwrite, EOF read, truncate and append pass; positions stay independent |
| 16 reads of 512 bytes | 16 sector reads retained, 1792 reopening by path |
| File flush before unmount | Disk FSInfo free count 129021; live count 128890 |
| Readonly fsck before unmount | Exit 1: dirty status and stale FSInfo summary |
| Readonly fsck after consuming unmount | Exit 0 |
| Independent mtools extraction | Both snapshots contain the same verified 66056-byte payload |

Sector counts describe this controlled image and byte/sector cursor, not target
latency or a kernel speedup. No allocation counts are claimed by this probe.
Dirty status on a mounted filesystem is expected, not evidence of payload
corruption. The clean stopped-image gate needs explicit terminal unmount or an
equivalent metadata-publication policy; file flush alone is not that boundary.
This PoC cursor forwards flush to `Sectors::sync`, unlike the kernel cursor.
The drop check proves a library flush callback, not unconditional physical writes
in every adapter. Retained handles must not move unexamined into OFD destruction.

Reproduce using a fresh owned directory; the binary refuses existing outputs:

```bash
work=$(mktemp -d /tmp/genrt-retained-handles.XXXXXX)
truncate -s 64M "$work/seed.img"
mkfs.fat --invariant -F 32 -S 512 -s 1 -i 00240024 -n GENRTPOC "$work/seed.img"
cargo test --manifest-path tests/storage/fat-poc/Cargo.toml --locked --doc
cargo run --manifest-path tests/storage/fat-poc/Cargo.toml --locked --release \
  --bin retained-handles -- "$work/seed.img" \
  "$work/before-unmount.img" "$work/after-unmount.img"
if fsck.fat -vn "$work/before-unmount.img"; then
  printf '%s\n' 'Expected pending filesystem metadata publication' >&2
  exit 1
else
  test "$?" -eq 1
fi
fsck.fat -vn "$work/after-unmount.img"
mcopy -i "$work/before-unmount.img" ::/CACHE/RETAIN.BIN "$work/before.bin"
mcopy -i "$work/after-unmount.img" ::/CACHE/RETAIN.BIN "$work/after.bin"
cmp "$work/before.bin" "$work/after.bin"
```

`src/retained.rs` has one positive Send check for an owning filesystem and four
expected compile failures: borrowed files are not Send, do not satisfy Any's
static lifetime, cannot escape a temporary filesystem, and prevent consuming
unmount while still in use. The memory device is Arc/Mutex-backed, so these
restrictions do not come from a deliberately non-Send device.

This establishes a useful canonical-handle direction, not a production-ready
replacement. Integration still needs an audited owner/dependent lifetime and
cross-CPU transfer model, authoritative metadata shared by independent opens,
bounded sync/unmount and error propagation, and non-I/O OFD cleanup. No kernel,
dependency API, accepted ownership decision or QEMU composition changes here.

The production backend subsequently adopts resident-fat with volume-owned
canonical files, not these borrowed starry handles. Small library allocations
are now permitted explicitly; see
[ADR-0067](../../../memory/decisions/ADR-0067-resident-fat-and-canonical-file-handles.md).
