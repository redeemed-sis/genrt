use fatfs::{Read, Seek, SeekFrom, Write};
use genrt_fat_poc_issue24::{Cursor, Error, Resident, Sectors};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering::Relaxed};

struct Meter;
static TRACK: AtomicBool = AtomicBool::new(false);
static ALLOC: AtomicUsize = AtomicUsize::new(0);
static FREE: AtomicUsize = AtomicUsize::new(0);
static REALLOC: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);
// SAFETY: every allocation is forwarded unchanged to System. Counters neither
// allocate nor dereference memory. This single-threaded harness brackets calls.
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let live = LIVE.fetch_add(l.size() as isize, Relaxed) + l.size() as isize;
            if TRACK.load(Relaxed) {
                ALLOC.fetch_add(1, Relaxed);
                PEAK.fetch_max(live, Relaxed);
            }
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if TRACK.load(Relaxed) {
            FREE.fetch_add(1, Relaxed);
        }
        LIVE.fetch_sub(l.size() as isize, Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, n) };
        if !q.is_null() {
            let live = LIVE.fetch_add(n as isize - l.size() as isize, Relaxed) + n as isize
                - l.size() as isize;
            if TRACK.load(Relaxed) {
                REALLOC.fetch_add(1, Relaxed);
                PEAK.fetch_max(live, Relaxed);
            }
        }
        q
    }
}
#[global_allocator]
static METER: Meter = Meter;
fn measured<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let base = LIVE.load(Relaxed);
    ALLOC.store(0, Relaxed);
    FREE.store(0, Relaxed);
    REALLOC.store(0, Relaxed);
    PEAK.store(base, Relaxed);
    TRACK.store(true, Relaxed);
    let result = f();
    TRACK.store(false, Relaxed);
    println!(
        "ALLOC {label}: alloc={} dealloc={} realloc={} peak_extra={} retained_delta={}",
        ALLOC.load(Relaxed),
        FREE.load(Relaxed),
        REALLOC.load(Relaxed),
        PEAK.load(Relaxed) - base,
        LIVE.load(Relaxed) - base
    );
    result
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Read,
    Write,
    Sync,
}
struct State {
    bytes: Vec<u8>,
    reads: usize,
    writes: usize,
    syncs: usize,
    fault: Option<(Op, usize)>,
    fault_hits: usize,
}
#[derive(Clone)]
struct Memory(Rc<RefCell<State>>);
impl Memory {
    fn load(path: &str) -> Self {
        Self(Rc::new(RefCell::new(State {
            bytes: std::fs::read(path).unwrap(),
            reads: 0,
            writes: 0,
            syncs: 0,
            fault: None,
            fault_hits: 0,
        })))
    }
    fn fail(&self, op: Op, after: usize) {
        self.0.borrow_mut().fault = Some((op, after));
    }
    fn clear(&self) {
        self.0.borrow_mut().fault = None;
    }
    fn save(&self, path: &str) {
        std::fs::write(path, &self.0.borrow().bytes).unwrap();
    }
    fn counts(&self, name: &str) {
        let s = self.0.borrow();
        println!(
            "DEVICE {name}: reads={} writes={} syncs={}",
            s.reads, s.writes, s.syncs
        );
    }
}
fn fault(s: &mut State, op: Op) -> Result<(), Error> {
    if let Some((kind, remaining)) = &mut s.fault
        && *kind == op
    {
        if *remaining == 0 {
            s.fault_hits += 1;
            return Err(Error::Injected);
        }
        *remaining -= 1;
    }
    Ok(())
}
fn range(s: &State, lba: u64, n: usize) -> Result<std::ops::Range<usize>, Error> {
    if !n.is_multiple_of(512) {
        return Err(Error::Bounds);
    }
    let start =
        usize::try_from(lba.checked_mul(512).ok_or(Error::Bounds)?).map_err(|_| Error::Bounds)?;
    let end = start
        .checked_add(n)
        .filter(|n| *n <= s.bytes.len())
        .ok_or(Error::Bounds)?;
    Ok(start..end)
}
impl Sectors for Memory {
    fn capacity(&self) -> u64 {
        self.0.borrow().bytes.len() as u64 / 512
    }
    fn read(&mut self, lba: u64, b: &mut [u8]) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        let r = range(&s, lba, b.len())?;
        fault(&mut s, Op::Read)?;
        s.reads += 1;
        b.copy_from_slice(&s.bytes[r]);
        Ok(())
    }
    fn write(&mut self, lba: u64, b: &[u8]) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        let r = range(&s, lba, b.len())?;
        fault(&mut s, Op::Write)?;
        s.writes += 1;
        s.bytes[r].copy_from_slice(b);
        Ok(())
    }
    fn sync(&mut self) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        fault(&mut s, Op::Sync)?;
        s.syncs += 1;
        Ok(())
    }
}
fn payload() -> Vec<u8> {
    (0..24593).map(|i| ((i * 31 + 7) % 251) as u8).collect()
}
fn checksum(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf29ce484222325, |a, b| {
        (a ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}
const LARGE: usize = 2 * 1024 * 1024 + 173;

fn resident(seed: &str, output: &str) {
    println!("=== resident-fat 0.3.1 ===");
    let mem = Memory::load(seed);
    let mut fs = measured("resident mount", || {
        resident_fat::FileSystem::mount(Resident(mem.clone())).unwrap()
    });
    println!("cluster_bytes={}", fs.boot_sector().cluster_bytes());
    assert!(fs.open("/missing").is_err());
    let mut data = payload();
    fs.create_dir("/poc").unwrap();
    let mut file = measured("resident create", || {
        fs.create("/poc/multi cluster.bin").unwrap()
    });
    measured("resident incremental grow 24KiB", || {
        for (i, part) in data.chunks(997).enumerate() {
            fs.write_at(&mut file, (i * 997) as u64, part).unwrap();
        }
    });
    let mut read = vec![0; data.len()];
    assert_eq!(fs.read_at(&file, 0, &mut read).unwrap(), data.len());
    assert_eq!(read, data);
    let mut small = [0; 4096];
    for n in [1, 512, 1024, 4096] {
        measured(&format!("resident warm read n={n} offset=3 x16"), || {
            for _ in 0..16 {
                assert_eq!(fs.read_at(&file, 3, &mut small[..n]).unwrap(), n);
            }
        });
        measured(&format!("resident overwrite n={n} offset=3 x16"), || {
            for _ in 0..16 {
                fs.write_at(&mut file, 3, &data[3..3 + n]).unwrap();
            }
        });
    }
    let other = fs.open("/poc/multi cluster.bin").unwrap();
    let tail = [0x5a; 6001];
    let end = file.len();
    measured("resident append 6001", || {
        fs.write_at(&mut file, u64::from(end), &tail).unwrap()
    });
    data.extend_from_slice(&tail);
    let old_len = other.len();
    let live_len = fs.open("/poc/multi cluster.bin").unwrap().len();
    let stale_eof = fs
        .read_at(&other, u64::from(old_len), &mut small[..1])
        .unwrap();
    println!(
        "OWNERSHIP resident old_handle_len={old_len} fresh_len={live_len} old_handle_read_append={stale_eof}"
    );
    assert_eq!(stale_eof, 0);
    // Canonical node handle with OFD-owned positions, fork shares the position owner.
    let shared_pos = Rc::new(RefCell::new(0u64));
    let fork_pos = shared_pos.clone();
    for pos in [&shared_pos, &fork_pos] {
        let mut at = pos.borrow_mut();
        let n = fs.read_at(&file, *at, &mut small[..13]).unwrap();
        assert_eq!(&small[..n], &data[*at as usize..*at as usize + n]);
        *at += n as u64;
    }
    assert_eq!(*shared_pos.borrow(), 26);
    assert_eq!(fs.read_at(&file, 0, &mut small[..13]).unwrap(), 13);
    assert_eq!(&small[..13], &data[..13]);
    measured("resident truncate 8197", || {
        fs.truncate(&mut file, 8197).unwrap()
    });
    data.truncate(8197);
    let names: Vec<_> = fs
        .open_dir("/poc")
        .unwrap()
        .iter()
        .map(|e| e.name().to_string())
        .collect();
    assert!(names.iter().any(|s| s == "multi cluster.bin"));
    println!("DIR resident {names:?}");
    assert_eq!(fs.read_at(&file, 8197, &mut small).unwrap(), 0);
    let mut large = fs.create("/poc/large.bin").unwrap();
    measured("resident incremental 2MiB+173 write", || {
        for at in (0..LARGE).step_by(4096) {
            let n = (LARGE - at).min(4096);
            fs.write_at(&mut large, at as u64, &[0x69; 4096][..n])
                .unwrap();
        }
    });
    measured("resident incremental 2MiB+173 read", || {
        for at in (0..LARGE).step_by(4096) {
            let n = (LARGE - at).min(4096);
            assert_eq!(fs.read_at(&large, at as u64, &mut small[..n]).unwrap(), n);
            assert!(small[..n].iter().all(|b| *b == 0x69));
        }
    });
    mem.fail(Op::Read, 0);
    assert!(fs.read_at(&file, 0, &mut small).is_err());
    mem.clear();
    mem.fail(Op::Write, 0);
    assert!(fs.write_at(&mut file, 0, &[1; 512]).is_err());
    mem.clear();
    assert_eq!(fs.read_at(&file, 0, &mut small[..512]).unwrap(), 512);
    assert_eq!(&small[..512], &data[..512]);
    measured("resident sync", || fs.sync().unwrap());
    println!(
        "SYNC resident crate_sync_does_not_call_device_flush={}",
        mem.0.borrow().syncs == 0
    );
    mem.fail(Op::Sync, 0);
    assert_eq!(mem.clone().sync(), Err(Error::Injected));
    mem.clear();
    let mut device = fs.unmount().unwrap();
    device.0.sync().unwrap();
    mem.save(output);
    let mut fs = resident_fat::FileSystem::mount(Resident(mem.clone())).unwrap();
    let file = fs.open("/poc/multi cluster.bin").unwrap();
    assert_eq!(fs.read_all(&file).unwrap(), data);
    println!(
        "PASS resident reopen bytes={} fnv64={:016x}",
        data.len(),
        checksum(&data)
    );
    fs.unmount().unwrap();
    mem.counts("resident");
}

fn starry(seed: &str, output: &str) {
    println!("=== starry-fatfs 0.4.1-preview.2 ===");
    let mem = Memory::load(seed);
    let fs = measured("starry mount", || {
        fatfs::FileSystem::new(
            Cursor::new(mem.clone()),
            fatfs::FsOptions::new().update_accessed_date(false),
        )
        .unwrap()
    });
    assert_eq!(fs.fat_type(), fatfs::FatType::Fat32);
    println!("cluster_bytes={}", fs.cluster_size());
    let mut data = payload();
    {
        let root = fs.root_dir();
        assert!(root.open_file("missing").is_err());
        let dir = root.create_dir("poc").unwrap();
        let mut file = measured("starry create", || {
            dir.create_file("multi cluster.bin").unwrap()
        });
        measured("starry incremental grow 24KiB", || {
            for part in data.chunks(997) {
                file.write_all(part).unwrap();
            }
        });
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut read = vec![0; data.len()];
        file.read_exact(&mut read).unwrap();
        assert_eq!(read, data);
        let mut small = [0; 4096];
        for n in [1, 512, 1024, 4096] {
            measured(&format!("starry warm read n={n} offset=3 x16"), || {
                for _ in 0..16 {
                    file.seek(SeekFrom::Start(3)).unwrap();
                    file.read_exact(&mut small[..n]).unwrap();
                }
            });
            measured(&format!("starry overwrite n={n} offset=3 x16"), || {
                for _ in 0..16 {
                    file.seek(SeekFrom::Start(3)).unwrap();
                    file.write_all(&data[3..3 + n]).unwrap();
                }
            });
        }
        file.flush().unwrap();
        let mut other = dir.open_file("multi cluster.bin").unwrap();
        let old_len = other.size().unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        let tail = [0x5a; 6001];
        measured("starry append 6001", || file.write_all(&tail).unwrap());
        data.extend_from_slice(&tail);
        file.flush().unwrap();
        other.seek(SeekFrom::End(0)).unwrap();
        let stale_eof = other.read(&mut small[..1]).unwrap();
        println!(
            "OWNERSHIP starry old_handle_len={old_len} fresh_len={} old_handle_read_append={stale_eof}",
            dir.open_file("multi cluster.bin").unwrap().size().unwrap()
        );
        assert_eq!(stale_eof, 0);
        drop(other);
        // One canonical file, position shared by fork, independent open starts at zero.
        let shared_pos = Rc::new(RefCell::new(0u64));
        let fork_pos = shared_pos.clone();
        for pos in [&shared_pos, &fork_pos] {
            let mut at = pos.borrow_mut();
            file.seek(SeekFrom::Start(*at)).unwrap();
            file.read_exact(&mut small[..13]).unwrap();
            assert_eq!(&small[..13], &data[*at as usize..*at as usize + 13]);
            *at += 13;
        }
        assert_eq!(*shared_pos.borrow(), 26);
        let mut independent = dir.open_file("multi cluster.bin").unwrap();
        independent.read_exact(&mut small[..13]).unwrap();
        assert_eq!(&small[..13], &data[..13]);
        drop(independent);
        file.seek(SeekFrom::Start(8197)).unwrap();
        measured("starry truncate 8197", || file.truncate().unwrap());
        data.truncate(8197);
        assert_eq!(file.read(&mut small).unwrap(), 0);
        file.seek(SeekFrom::Start(0)).unwrap();
        mem.fail(Op::Read, 0);
        assert!(file.read(&mut small).is_err());
        mem.clear();
        file.seek(SeekFrom::Start(0)).unwrap();
        mem.fail(Op::Write, 0);
        assert!(file.write(&[1; 512]).is_err());
        mem.clear();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.read_exact(&mut small[..512]).unwrap();
        assert_eq!(&small[..512], &data[..512]);
        mem.fail(Op::Sync, 0);
        assert!(file.flush().is_err());
        mem.clear();
        measured("starry file flush", || file.flush().unwrap());
        for n in [1, 512, 1024, 4096] {
            measured(
                &format!("starry reconstruct+seek+read+drop n={n} x16"),
                || {
                    for _ in 0..16 {
                        let mut f = fs.root_dir().open_file("poc/multi cluster.bin").unwrap();
                        f.seek(SeekFrom::Start(3)).unwrap();
                        f.read_exact(&mut small[..n]).unwrap();
                        assert_eq!(&small[..n], &data[3..3 + n]);
                    }
                },
            );
            measured(
                &format!("starry reconstruct+seek+write+flush+drop n={n} x16"),
                || {
                    for _ in 0..16 {
                        let mut f = fs.root_dir().open_file("poc/multi cluster.bin").unwrap();
                        f.seek(SeekFrom::Start(3)).unwrap();
                        f.write_all(&data[3..3 + n]).unwrap();
                        f.flush().unwrap();
                    }
                },
            );
        }
        let mut large = dir.create_file("large.bin").unwrap();
        measured("starry incremental 2MiB+173 write", || {
            for at in (0..LARGE).step_by(4096) {
                let n = (LARGE - at).min(4096);
                large.write_all(&[0x69; 4096][..n]).unwrap();
            }
        });
        large.seek(SeekFrom::Start(0)).unwrap();
        measured("starry incremental 2MiB+173 read", || {
            for at in (0..LARGE).step_by(4096) {
                let n = (LARGE - at).min(4096);
                large.read_exact(&mut small[..n]).unwrap();
                assert!(small[..n].iter().all(|b| *b == 0x69));
            }
        });
        large.flush().unwrap();
        drop(large);
        let names: Vec<_> = dir
            .iter()
            .map(|e| {
                let e = e.unwrap();
                if let Some(name) = e.long_file_name_as_ucs2_units() {
                    String::from_utf16_lossy(name)
                } else {
                    String::from_utf8_lossy(e.short_file_name_as_bytes()).into_owned()
                }
            })
            .collect();
        assert!(names.iter().any(|s| s == "multi cluster.bin"));
        println!("DIR starry {names:?}");
    }
    measured("starry unmount", || fs.unmount().unwrap());
    // unmount writes metadata after file flush and has no final disk.flush call.
    mem.clone().sync().unwrap();
    mem.save(output);
    let fs = fatfs::FileSystem::new(
        Cursor::new(mem.clone()),
        fatfs::FsOptions::new().update_accessed_date(false),
    )
    .unwrap();
    {
        let mut file = fs.root_dir().open_file("poc/multi cluster.bin").unwrap();
        let mut read = vec![0; data.len()];
        file.read_exact(&mut read).unwrap();
        assert_eq!(read, data);
        assert_eq!(file.read(&mut [0; 1]).unwrap(), 0);
    }
    fs.unmount().unwrap();
    println!(
        "PASS starry reopen bytes={} fnv64={:016x}",
        data.len(),
        checksum(&data)
    );
    mem.counts("starry");
}

fn negatives(seed: &str) {
    let mem = Memory::load(seed);
    let mut cur = Cursor::new(mem.clone());
    assert!(cur.seek(SeekFrom::Start(u64::MAX)).is_err());
    assert!(cur.seek(SeekFrom::Current(-1)).is_err());
    cur.seek(SeekFrom::End(0)).unwrap();
    assert_eq!(cur.read(&mut [0; 1]).unwrap(), 0);
    assert_eq!(cur.write(&[1]).unwrap(), 0);
    assert!(mem.clone().read(mem.capacity(), &mut [0; 512]).is_err());
    mem.fail(Op::Read, 0);
    assert!(resident_fat::FileSystem::mount(Resident(mem.clone())).is_err());
    assert!(fatfs::FileSystem::new(Cursor::new(mem.clone()), fatfs::FsOptions::new()).is_err());
    mem.clear();
    mem.0.borrow_mut().bytes[510] = 0;
    assert!(resident_fat::FileSystem::mount(Resident(mem.clone())).is_err());
    assert!(fatfs::FileSystem::new(Cursor::new(mem), fatfs::FsOptions::new()).is_err());
    println!(
        "PASS negatives: seek under/overflow, capacity, EOF, mount read fault, bad boot signature"
    );
}

fn partial_and_ownership(seed: &str) {
    let mem = Memory::load(seed);
    let mut fs = resident_fat::FileSystem::mount(Resident(mem.clone())).unwrap();
    let mut a = fs.write_file("/partial.bin", &[0x11; 8192]).unwrap();
    // One 8-sector write succeeds, the following one fails. The API returns
    // only Error, even though the first half of this call is committed.
    mem.fail(Op::Write, 1);
    let result = fs.write_at(&mut a, 0, &[0x22; 8192]);
    assert!(result.is_err());
    mem.clear();
    let read = fs.read_all(&a).unwrap();
    assert!(read[..4096].iter().all(|b| *b == 0x22));
    assert!(read[4096..].iter().all(|b| *b == 0x11));
    println!(
        "PARTIAL resident: write_at returned {result:?}, first 4096/8192 bytes changed; no progress count in API"
    );
    let mut a = fs.write_file("/owners.bin", &[0x33; 1024]).unwrap();
    let mut b = fs.open("/owners.bin").unwrap();
    fs.write_at(&mut a, 1024, &[0x44; 1024]).unwrap();
    fs.write_at(&mut b, 1024, &[0x55]).unwrap();
    println!(
        "STALE-WRITER resident: expected shared length 2048, actual directory length {}",
        fs.open("/owners.bin").unwrap().len()
    );
    assert_eq!(fs.open("/owners.bin").unwrap().len(), 1025);
    // Separate scratch instance is discarded, never the clean result image.
    drop(fs);

    let mem = Memory::load(seed);
    let fs = fatfs::FileSystem::new(
        Cursor::new(mem.clone()),
        fatfs::FsOptions::new().update_accessed_date(false),
    )
    .unwrap();
    {
        let root = fs.root_dir();
        let mut a = root.create_file("partial.bin").unwrap();
        a.write_all(&[0x11; 8192]).unwrap();
        a.flush().unwrap();
        a.seek(SeekFrom::Start(0)).unwrap();
        mem.fail(Op::Write, 1);
        let n = a.write(&[0x22; 8192]).unwrap();
        assert_eq!(n, 512);
        assert!(a.write(&[0x22; 8192 - 512]).is_err());
        mem.clear();
        assert_eq!(a.seek(SeekFrom::Current(0)).unwrap(), 512);
        println!(
            "PARTIAL starry: write returned 512, next write returned error; caller can retain exact progress"
        );
        let mut a = root.create_file("owners.bin").unwrap();
        a.write_all(&[0x33; 1024]).unwrap();
        a.flush().unwrap();
        let mut b = root.open_file("owners.bin").unwrap();
        a.write_all(&[0x44; 1024]).unwrap();
        a.flush().unwrap();
        b.seek(SeekFrom::End(0)).unwrap();
        b.write_all(&[0x55]).unwrap();
        b.flush().unwrap();
        let actual = root.open_file("owners.bin").unwrap().size().unwrap();
        println!(
            "STALE-WRITER starry: expected shared length 2048, actual directory length {actual}"
        );
        assert_eq!(actual, 1025);
        b.seek(SeekFrom::Start(0)).unwrap();
        assert!(b.seek(SeekFrom::Current(-2)).is_err());
        assert_eq!(b.seek(SeekFrom::Start(9999)).unwrap(), 1025);
        println!(
            "SEEK starry: beyond EOF clamps to EOF; adapter must implement zero-extension separately"
        );
    }
    fs.unmount().unwrap();
}

fn disk_full(seed: &str) {
    let mem = Memory::load(seed);
    let mut fs = resident_fat::FileSystem::mount(Resident(mem)).unwrap();
    let mut f = fs.create("/full.bin").unwrap();
    assert!(fs.truncate(&mut f, u32::MAX).is_err());
    println!("PASS resident disk-full rejected oversized extension");
    drop(fs);
    let mem = Memory::load(seed);
    let fs = fatfs::FileSystem::new(Cursor::new(mem), fatfs::FsOptions::new()).unwrap();
    {
        let mut f = fs.root_dir().create_file("full.bin").unwrap();
        let block = [0x66; 4096];
        let mut done = 0usize;
        loop {
            match f.write(&block) {
                Ok(0) => panic!("unexpected zero before disk-full error"),
                Ok(n) => done += n,
                Err(fatfs::Error::NotEnoughSpace) => break,
                Err(e) => panic!("unexpected disk-full error {e:?}"),
            }
        }
        assert!(done > 60 * 1024 * 1024);
        assert_eq!(f.size(), Some(done as u32));
        f.flush().unwrap();
        println!("PASS starry disk-full after {done} bytes with exact successful progress");
    }
    fs.unmount().unwrap();
}

fn cross_read(resident: &str, starry: &str) {
    let mem = Memory::load(starry);
    let mut fs = resident_fat::FileSystem::mount(Resident(mem)).unwrap();
    let f = fs.open("/poc/multi cluster.bin").unwrap();
    let a = fs.read_all(&f).unwrap();
    fs.unmount().unwrap();
    let mem = Memory::load(resident);
    let fs = fatfs::FileSystem::new(Cursor::new(mem), fatfs::FsOptions::new()).unwrap();
    let mut b = vec![0; 8197];
    {
        let mut f = fs.root_dir().open_file("poc/multi cluster.bin").unwrap();
        f.read_exact(&mut b).unwrap();
    }
    fs.unmount().unwrap();
    assert_eq!(a, b);
    let parent = std::path::Path::new(resident).parent().unwrap();
    std::fs::write(parent.join("from-starry-via-resident.bin"), a).unwrap();
    std::fs::write(parent.join("from-resident-via-starry.bin"), b).unwrap();
    println!("PASS independent-library cross-read of both persisted images");
}

fn short_lived_volume(seed: &str) {
    let mut mem = Memory::load(seed);
    let path = "long directory/persistent long file.bin";
    let data = payload();
    {
        let fs = fatfs::FileSystem::new(
            Cursor::new(&mut mem),
            fatfs::FsOptions::new().update_accessed_date(false),
        )
        .unwrap();
        {
            let dir = fs.root_dir().create_dir("long directory").unwrap();
            let mut f = dir.create_file("persistent long file.bin").unwrap();
            f.write_all(&data).unwrap();
            f.flush().unwrap();
        }
        fs.unmount().unwrap();
    }
    mem.sync().unwrap();
    let mut small = [0; 4096];
    for n in [1, 512, 1024, 4096] {
        measured(
            &format!("short-lived mount+LFN open+read+unmount+outer sync n={n} x16"),
            || {
                for _ in 0..16 {
                    let fs = fatfs::FileSystem::new(
                        Cursor::new(&mut mem),
                        fatfs::FsOptions::new().update_accessed_date(false),
                    )
                    .unwrap();
                    {
                        let mut f = fs.root_dir().open_file(path).unwrap();
                        f.seek(SeekFrom::Start(3)).unwrap();
                        f.read_exact(&mut small[..n]).unwrap();
                        assert_eq!(&small[..n], &data[3..3 + n]);
                        f.flush().unwrap();
                    }
                    fs.unmount().unwrap();
                    mem.sync().unwrap();
                }
            },
        );
        measured(
            &format!("short-lived mount+LFN open+write+unmount+outer sync n={n} x16"),
            || {
                for _ in 0..16 {
                    let fs = fatfs::FileSystem::new(
                        Cursor::new(&mut mem),
                        fatfs::FsOptions::new().update_accessed_date(false),
                    )
                    .unwrap();
                    {
                        let mut f = fs.root_dir().open_file(path).unwrap();
                        f.seek(SeekFrom::Start(3)).unwrap();
                        f.write_all(&data[3..3 + n]).unwrap();
                        f.flush().unwrap();
                    }
                    fs.unmount().unwrap();
                    mem.sync().unwrap();
                }
            },
        );
    }
    // Each mutation reconstructs from the latest published metadata.
    measured(
        "short-lived append then truncate then read, separate mounts",
        || {
            let fs = fatfs::FileSystem::new(
                Cursor::new(&mut mem),
                fatfs::FsOptions::new().update_accessed_date(false),
            )
            .unwrap();
            {
                let mut f = fs.root_dir().open_file(path).unwrap();
                assert_eq!(f.seek(SeekFrom::End(0)).unwrap(), data.len() as u64);
                f.write_all(&[0x77; 513]).unwrap();
                f.flush().unwrap();
            }
            fs.unmount().unwrap();
            mem.sync().unwrap();
            let fs = fatfs::FileSystem::new(
                Cursor::new(&mut mem),
                fatfs::FsOptions::new().update_accessed_date(false),
            )
            .unwrap();
            {
                let mut f = fs.root_dir().open_file(path).unwrap();
                assert_eq!(f.size(), Some((data.len() + 513) as u32));
                f.seek(SeekFrom::Start(4197)).unwrap();
                f.truncate().unwrap();
                f.flush().unwrap();
            }
            fs.unmount().unwrap();
            mem.sync().unwrap();
            let fs = fatfs::FileSystem::new(
                Cursor::new(&mut mem),
                fatfs::FsOptions::new().update_accessed_date(false),
            )
            .unwrap();
            {
                let mut f = fs.root_dir().open_file(path).unwrap();
                assert_eq!(f.size(), Some(4197));
                f.read_exact(&mut small).unwrap();
                assert_eq!(&small, &data[..4096]);
                f.flush().unwrap();
            }
            fs.unmount().unwrap();
            mem.sync().unwrap();
        },
    );
    let parent = std::path::Path::new(seed).parent().unwrap();
    mem.save(parent.join("short-lived-starry.img").to_str().unwrap());
    // Force dirty FSInfo. All handles are gone before injecting unmount errors.
    let control = mem.clone();
    let fs = fatfs::FileSystem::new(Cursor::new(&mut mem), fatfs::FsOptions::new()).unwrap();
    {
        let mut f = fs.root_dir().create_file("unmount fault.bin").unwrap();
        f.write_all(&[0x88; 1024]).unwrap();
        f.flush().unwrap();
    }
    let before = control.0.borrow().fault_hits;
    control.fail(Op::Write, 0);
    let result = measured("short-lived unmount persistent write error", || {
        fs.unmount()
    });
    assert!(matches!(result, Err(fatfs::Error::Io(Error::Injected))));
    assert_eq!(control.0.borrow().fault_hits - before, 2);
    control.clear();
    println!(
        "UNMOUNT error is returned; Drop retries once (2 injected failures). Borrowed device is returned, but FS owner/cache consumed; do not report sync success."
    );
    // A clean unmount does NOT call the transport's flush. The outer barrier
    // must run after metadata writes and its error must be observable.
    let before = control.0.borrow().syncs;
    let fs = fatfs::FileSystem::new(Cursor::new(&mut mem), fatfs::FsOptions::new()).unwrap();
    fs.unmount().unwrap();
    assert_eq!(control.0.borrow().syncs, before);
    control.fail(Op::Sync, 0);
    assert_eq!(mem.sync(), Err(Error::Injected));
    control.clear();
    mem.sync().unwrap();
    println!(
        "PASS short-lived borrowed-volume operations, fresh append/truncate lengths, unmount and outer-sync errors"
    );
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 4, "usage: poc seed.img resident.img starry.img");
    resident(&args[1], &args[2]);
    starry(&args[1], &args[3]);
    negatives(&args[1]);
    partial_and_ownership(&args[1]);
    disk_full(&args[1]);
    cross_read(&args[2], &args[3]);
    short_lived_volume(&args[1]);
}
