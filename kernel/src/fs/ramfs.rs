pub struct RamFile {
    pub path: &'static [u8],
    pub data: &'static [u8],
}

static RAMFS_FILES: &[RamFile] = &[
    RamFile {
        path: b"/hello.txt",
        data: b"hello from ramfs\n",
    },
    RamFile {
        path: b"/etc/banner",
        data: b"genrt ramfs\n",
    },
];

pub fn lookup(path: &[u8]) -> Option<usize> {
    RAMFS_FILES
        .iter()
        .enumerate()
        .find_map(|(index, file)| (file.path == path).then_some(index))
}

pub fn data(index: usize) -> Option<&'static [u8]> {
    RAMFS_FILES.get(index).map(|file| file.data)
}
