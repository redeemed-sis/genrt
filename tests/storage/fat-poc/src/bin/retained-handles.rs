//! Executable persistent-mount probe. Never formats or overwrites an image.

use fatfs::{Read, Seek, SeekFrom, Write};
use genrt_fat_poc_issue24::{
    Cursor, Error, Sectors,
    retained::{File, System},
};
use std::{
    io::Write as _,
    path::Path,
    sync::{Arc, Mutex},
};

struct State {
    bytes: Vec<u8>,
    reads: usize,
    writes: usize,
    syncs: usize,
}

#[derive(Clone)]
struct Memory(Arc<Mutex<State>>);

impl Memory {
    fn load(path: &Path) -> Self {
        Self(Arc::new(Mutex::new(State {
            bytes: std::fs::read(path).unwrap(),
            reads: 0,
            writes: 0,
            syncs: 0,
        })))
    }

    fn counts(&self) -> (usize, usize, usize) {
        let state = self.0.lock().unwrap();
        (state.reads, state.writes, state.syncs)
    }

    fn free_clusters_on_disk(&self) -> u32 {
        let state = self.0.lock().unwrap();
        let sector = u16::from_le_bytes(state.bytes[48..50].try_into().unwrap());
        let at = usize::from(sector) * 512 + 488;
        u32::from_le_bytes(state.bytes[at..at + 4].try_into().unwrap())
    }

    fn save_new(&self, path: &Path) {
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        output.write_all(&self.0.lock().unwrap().bytes).unwrap();
        output.sync_all().unwrap();
    }
}

fn range(bytes: &[u8], sector: u64, length: usize) -> Result<std::ops::Range<usize>, Error> {
    if !length.is_multiple_of(512) {
        return Err(Error::Bounds);
    }
    let start = usize::try_from(sector.checked_mul(512).ok_or(Error::Bounds)?)
        .map_err(|_| Error::Bounds)?;
    let end = start
        .checked_add(length)
        .filter(|end| *end <= bytes.len())
        .ok_or(Error::Bounds)?;
    Ok(start..end)
}

impl Sectors for Memory {
    fn capacity(&self) -> u64 {
        (self.0.lock().unwrap().bytes.len() / 512) as u64
    }

    fn read(&mut self, sector: u64, bytes: &mut [u8]) -> Result<(), Error> {
        let mut state = self.0.lock().unwrap();
        let range = range(&state.bytes, sector, bytes.len())?;
        bytes.copy_from_slice(&state.bytes[range]);
        state.reads += 1;
        Ok(())
    }

    fn write(&mut self, sector: u64, bytes: &[u8]) -> Result<(), Error> {
        let mut state = self.0.lock().unwrap();
        let range = range(&state.bytes, sector, bytes.len())?;
        state.bytes[range].copy_from_slice(bytes);
        state.writes += 1;
        Ok(())
    }

    fn sync(&mut self) -> Result<(), Error> {
        self.0.lock().unwrap().syncs += 1;
        Ok(())
    }
}

struct Node<'fs> {
    id: u64,
    file: File<'fs, Memory>,
}

struct Open {
    node: u64,
    offset: u64,
}

fn write(node: &mut Node<'_>, open: &mut Open, bytes: &[u8], append: bool) {
    assert_eq!(node.id, open.node);
    let offset = if append {
        u64::from(node.file.size().unwrap())
    } else {
        open.offset
    };
    // This probe exercises existing extents/append, not sparse extension.
    assert!(offset <= u64::from(node.file.size().unwrap()));
    assert_eq!(node.file.seek(SeekFrom::Start(offset)).unwrap(), offset);
    node.file.write_all(bytes).unwrap();
    node.file.flush().unwrap();
    open.offset = offset + bytes.len() as u64;
}

fn read(node: &mut Node<'_>, open: &mut Open, bytes: &mut [u8]) -> usize {
    assert_eq!(node.id, open.node);
    if open.offset >= u64::from(node.file.size().unwrap()) {
        return 0;
    }
    assert_eq!(
        node.file.seek(SeekFrom::Start(open.offset)).unwrap(),
        open.offset
    );
    let mut count = 0;
    while count < bytes.len() {
        let got = node.file.read(&mut bytes[count..]).unwrap();
        if got == 0 {
            break;
        }
        count += got;
    }
    open.offset += count as u64;
    count
}

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    assert_eq!(
        args.len(),
        4,
        "retained-handles SEED BEFORE_UNMOUNT AFTER_UNMOUNT"
    );
    let memory = Memory::load(Path::new(&args[1]));
    let fs = System::new(Cursor::new(memory.clone()), fatfs::FsOptions::new()).unwrap();
    assert_eq!(fs.fat_type(), fatfs::FatType::Fat32);
    let directory = fs.root_dir().create_dir("CACHE").unwrap();

    // Two independent library handles reproduce stale metadata, despite fully
    // sequential access and flushing. Repair the probe file before snapshots.
    let mut first = directory.create_file("STALE.BIN").unwrap();
    first.write_all(&[0x11; 1024]).unwrap();
    first.flush().unwrap();
    let mut second = directory.open_file("STALE.BIN").unwrap();
    first.seek(SeekFrom::End(0)).unwrap();
    first.write_all(&[0x22; 1024]).unwrap();
    first.flush().unwrap();
    second.seek(SeekFrom::Start(1024)).unwrap();
    second.write_all(&[0x33]).unwrap();
    second.flush().unwrap();
    drop(second);
    drop(first);
    let mut repaired = directory.open_file("STALE.BIN").unwrap();
    assert_eq!(repaired.size(), Some(1025));
    println!("Independent library handles: expected length 2048, observed 1025");
    repaired.seek(SeekFrom::Start(0)).unwrap();
    repaired.truncate().unwrap();
    repaired.flush().unwrap();
    drop(repaired);

    // One authoritative library handle, two independent VFS-style positions.
    let mut node = Node {
        id: 2,
        file: directory.create_file("RETAIN.BIN").unwrap(),
    };
    let mut a = Open { node: 2, offset: 0 };
    let mut b = Open { node: 2, offset: 0 };
    write(&mut node, &mut a, &[0x11; 1024], false);
    b.offset = 1024;
    write(&mut node, &mut b, &[0x22; 1024], false);
    a.offset = 1024;
    write(&mut node, &mut a, &[0x33], false);
    assert_eq!(node.file.size(), Some(2048));
    assert_eq!((a.offset, b.offset), (1025, 2048));
    node.file.seek(SeekFrom::Start(1024)).unwrap();
    let mut overwritten = [0];
    node.file.read_exact(&mut overwritten).unwrap();
    assert_eq!(overwritten, [0x33]);
    node.file.seek(SeekFrom::Start(513)).unwrap();
    node.file.truncate().unwrap();
    node.file.flush().unwrap();
    let mut data = [0; 4096];
    assert_eq!(read(&mut node, &mut b, &mut data), 0);
    write(&mut node, &mut b, &[0x44; 7], true);
    assert_eq!(node.file.size(), Some(520));
    assert_eq!((a.offset, b.offset), (1025, 520));
    for _ in 0..16 {
        write(&mut node, &mut b, &[0x55; 4096], true);
    }
    println!("Canonical handle: independent offsets, overwrite, truncate and append passed");

    let before = memory.counts().0;
    for _ in 0..16 {
        a.offset = 0;
        assert_eq!(read(&mut node, &mut a, &mut data[..512]), 512);
    }
    let retained_reads = memory.counts().0 - before;
    let before = memory.counts().0;
    for _ in 0..16 {
        let mut file = fs
            .root_dir()
            .open_dir("CACHE")
            .unwrap()
            .open_file("RETAIN.BIN")
            .unwrap();
        file.read_exact(&mut data[..512]).unwrap();
    }
    let reopened_reads = memory.counts().0 - before;
    assert!(retained_reads < reopened_reads);
    println!("Sector reads, 16 x 512 bytes: retained={retained_reads}, reopen={reopened_reads}");

    node.file.flush().unwrap();
    let expected = fs.stats().unwrap().free_clusters();
    let old = memory.free_clusters_on_disk();
    assert_ne!(old, u32::MAX, "seed needs a known FSInfo free count");
    assert_ne!(old, expected);
    memory.save_new(Path::new(&args[2]));
    println!("After File::flush: FSInfo free clusters on disk={old}, live={expected}");

    let before = memory.counts().2;
    drop(node);
    assert!(
        memory.counts().2 > before,
        "File::drop invokes cursor flush even after explicit flush"
    );
    println!("File::drop calls the transport flush through this PoC cursor");
    drop(directory);
    fs.unmount().unwrap();
    assert_eq!(memory.free_clusters_on_disk(), expected);
    memory.clone().sync().unwrap();
    memory.save_new(Path::new(&args[3]));

    let reopened = System::new(Cursor::new(memory), fatfs::FsOptions::new()).unwrap();
    let mut file = reopened
        .root_dir()
        .open_dir("CACHE")
        .unwrap()
        .open_file("RETAIN.BIN")
        .unwrap();
    assert_eq!(file.size(), Some(66056));
    let mut bytes = vec![0; 66056];
    file.read_exact(&mut bytes).unwrap();
    assert!(bytes[..513].iter().all(|b| *b == 0x11));
    assert!(bytes[513..520].iter().all(|b| *b == 0x44));
    assert!(bytes[520..].iter().all(|b| *b == 0x55));
    drop(file);
    reopened.unmount().unwrap();
    println!("Consuming unmount publishes FSInfo; reopened payload verified (66056 bytes)");
}
