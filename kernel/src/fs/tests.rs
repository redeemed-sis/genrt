//! Focused pure host contracts for namespace, backend, OFD and failure ownership.

use super::{
    backend::*,
    fd::{FdTable, Open, Transaction},
    ramfs::{MountedRamfs, RamFile},
    testfs::TestFs,
    vfs::{Location, Namespace, Resolver},
};
use crate::errno;
use alloc::{sync::Arc, vec, vec::Vec};
use core::task::Poll;

fn root(directories: Vec<Vec<u8>>) -> Arc<dyn Backend> {
    Arc::new(
        MountedRamfs::new(
            vec![RamFile {
                path: b"/hello".to_vec(),
                data: b"hello",
            }],
            directories,
        )
        .unwrap(),
    )
}

fn fixture() -> (Namespace, Arc<TestFs>, Arc<TestFs>) {
    let mut namespace = Namespace::new(root(vec![b"/m".to_vec(), b"/n".to_vec()])).unwrap();
    let a = Arc::new(TestFs::new("a"));
    let b = Arc::new(TestFs::new("b"));
    namespace.mount(b"/m", a.clone()).unwrap();
    namespace.mount(b"/n", b.clone()).unwrap();
    (namespace, a, b)
}

fn resolve(ns: &Namespace, cwd: Location, path: &[u8]) -> Result<Location, isize> {
    match Resolver::new(cwd, path, false)?.poll(ns, None) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("unexpected pending resolver"),
    }
}

fn flags() -> OpenFlags {
    OpenFlags {
        read: true,
        ..OpenFlags::default()
    }
}

fn open(ns: &Namespace, path: &[u8], flags: OpenFlags) -> Arc<super::fd::OpenDescription> {
    let mut open = Open::new(ns.root(), path, flags).unwrap();
    match open.poll(ns, None) {
        Poll::Ready(result) => result.unwrap(),
        Poll::Pending => panic!("unexpected pending open"),
    }
}

fn complete(tx: &mut Transaction) -> Result<Reply, isize> {
    match tx.poll(None) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("unexpected pending IO"),
    }
}

fn read(fd: Arc<super::fd::OpenDescription>, count: usize, commit: bool) -> Vec<u8> {
    let mut tx = Transaction::new(
        fd,
        Operation::Read {
            offset: 0,
            limit: count,
        },
        count,
    )
    .unwrap();
    let Reply::Io { count, .. } = complete(&mut tx).unwrap() else {
        panic!("not IO");
    };
    let result = tx.buffer()[..count].to_vec();
    if commit {
        tx.commit();
    }
    result
}

fn write(fd: Arc<super::fd::OpenDescription>, data: &[u8]) {
    let mut tx = Transaction::new(
        fd,
        Operation::Write {
            offset: 0,
            append: false,
        },
        data.len(),
    )
    .unwrap();
    assert!(matches!(
        tx.prepare(None, |buffer| {
            buffer.copy_from_slice(data);
            Ok(())
        }),
        Poll::Ready(Ok(()))
    ));
    assert!(matches!(complete(&mut tx), Ok(Reply::Io { count, .. }) if count == data.len()));
    tx.commit();
}

#[test]
fn traversal_mount_parent_relative_directory_and_conflicts() {
    let (mut ns, a, _) = fixture();
    let sub = resolve(&ns, ns.root(), b"/m/sub").unwrap();
    assert_eq!(
        resolve(&ns, sub.clone(), b"..//../n/./dynamic")
            .unwrap()
            .path(),
        b"/n/dynamic"
    );
    assert_eq!(
        resolve(&ns, sub, b"/../../m/../hello").unwrap().path(),
        b"/hello"
    );
    assert!(matches!(
        resolve(&ns, ns.root(), b"/missing/../hello"),
        Err(errno::ENOENT)
    ));
    assert!(matches!(
        resolve(&ns, ns.root(), b"/hello/../m"),
        Err(errno::ENOTDIR)
    ));
    assert!(matches!(
        resolve(&ns, ns.root(), b"/m/dynamic/"),
        Err(errno::ENOTDIR)
    ));
    assert_eq!(
        ns.mount(b"/m", Arc::new(TestFs::new("c"))),
        Err(errno::EBUSY)
    );
    assert_eq!(ns.mount(b"/m/sub", a), Err(errno::EINVAL));
    assert_eq!(
        ns.mount(b"relative", Arc::new(TestFs::new("c"))),
        Err(errno::EINVAL)
    );
    assert_eq!(
        ns.mount(b"/hello", Arc::new(TestFs::new("c"))),
        Err(errno::ENOTDIR)
    );
    assert_eq!(
        ns.mount(b"/missing", Arc::new(TestFs::new("c"))),
        Err(errno::ENOENT)
    );
    ns.mount(b"/m/sub", Arc::new(TestFs::new("nested")))
        .unwrap();
    let nested = resolve(&ns, ns.root(), b"/m/sub/sub/../..").unwrap();
    assert_eq!(nested.path(), b"/m");
}

#[test]
fn ancestor_mount_cannot_shadow_an_existing_mount() {
    let mut ns = Namespace::new(root(vec![b"/outer/inner".to_vec()])).unwrap();
    ns.mount(b"/outer/inner", Arc::new(TestFs::new("inner")))
        .unwrap();
    assert_eq!(
        ns.mount(b"/outer", Arc::new(TestFs::new("outer"))),
        Err(errno::EBUSY)
    );
}

#[test]
fn readonly_errors_and_generic_exec_preserve_archive_bytes() {
    let (ns, _, _) = fixture();
    let mut op = Open::new(
        ns.root(),
        b"/hello",
        OpenFlags {
            write: true,
            ..flags()
        },
    )
    .unwrap();
    assert!(matches!(op.poll(&ns, None), Poll::Ready(Err(errno::EROFS))));
    let mut op = Open::new(
        ns.root(),
        b"/missing",
        OpenFlags {
            create: true,
            write: true,
            ..flags()
        },
    )
    .unwrap();
    assert!(matches!(op.poll(&ns, None), Poll::Ready(Err(errno::EROFS))));
    let fd = open(&ns, b"/hello", flags());
    assert!(matches!(
        Transaction::new(
            fd.clone(),
            Operation::Write {
                offset: 0,
                append: false
            },
            1
        ),
        Err(errno::EBADF)
    ));
    assert!(matches!(
        Transaction::new(fd, Operation::Truncate { size: 0 }, 0),
        Err(errno::EBADF)
    ));
    assert_eq!(
        resolve(&ns, ns.root(), b"/hello").unwrap().executable(),
        Some(b"hello".as_slice())
    );
    assert!(
        resolve(&ns, ns.root(), b"/m/dynamic")
            .unwrap()
            .executable()
            .is_none()
    );
}

#[test]
fn fork_tables_share_exact_offsets_but_separate_opens_do_not() {
    let (ns, _, _) = fixture();
    let mut parent = FdTable::new();
    assert_eq!(
        parent.install(&mut Some(open(&ns, b"/hello", flags()))),
        Ok(3)
    );
    let mut child = parent.clone();
    assert!(Arc::ptr_eq(&parent.get(3).unwrap(), &child.get(3).unwrap()));
    assert_eq!(read(parent.get(3).unwrap(), 2, true), b"he");
    assert_eq!(read(child.get(3).unwrap(), 2, true), b"ll");
    assert_eq!(read(open(&ns, b"/hello", flags()), 2, true), b"he");
    drop(parent.take(3).unwrap());
    assert_eq!(read(child.get(3).unwrap(), 2, true), b"o");
    drop(child.take(3).unwrap());
    assert_eq!(child.install(&mut Some(open(&ns, b"/m", flags()))), Ok(3));
    let copied = child.clone();
    let mut tx =
        Transaction::new(child.get(3).unwrap(), Operation::Readdir { offset: 0 }, 0).unwrap();
    let Reply::Entry(Some(first)) = complete(&mut tx).unwrap() else {
        panic!("entry");
    };
    tx.commit();
    drop(tx);
    let mut tx =
        Transaction::new(copied.get(3).unwrap(), Operation::Readdir { offset: 0 }, 0).unwrap();
    let Reply::Entry(Some(second)) = complete(&mut tx).unwrap() else {
        panic!("entry");
    };
    assert_ne!(first.name, second.name);
}

#[test]
fn unsuccessful_copy_rolls_back_file_and_directory_positions() {
    let (ns, _, _) = fixture();
    let fd = open(&ns, b"/hello", flags());
    assert_eq!(read(fd.clone(), 2, false), b"he");
    assert_eq!(read(fd, 2, true), b"he");
    let fd = open(&ns, b"/m", flags());
    let mut tx = Transaction::new(fd.clone(), Operation::Readdir { offset: 0 }, 0).unwrap();
    let Reply::Entry(Some(first)) = complete(&mut tx).unwrap() else {
        panic!("entry");
    };
    drop(tx); // EFAULT/too-small output never calls commit.
    let mut tx = Transaction::new(fd, Operation::Readdir { offset: 0 }, 0).unwrap();
    let Reply::Entry(Some(retry)) = complete(&mut tx).unwrap() else {
        panic!("entry");
    };
    assert_eq!(first.name, retry.name);
}

#[test]
fn create_truncate_append_seek_sync_stat_and_instance_isolation() {
    let (ns, _, _) = fixture();
    let writable = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..OpenFlags::default()
    };
    let fd = open(&ns, b"/m/new", writable);
    write(fd.clone(), b"abc");
    write(fd, b"def");
    let first = open(
        &ns,
        b"/m/new",
        OpenFlags {
            append: true,
            ..writable
        },
    );
    let second = open(
        &ns,
        b"/m/new",
        OpenFlags {
            append: true,
            ..writable
        },
    );
    write(first, b"1");
    write(second, b"2");
    assert_eq!(read(open(&ns, b"/m/new", flags()), 99, true), b"abcdef12");
    assert!(matches!(
        resolve(&ns, ns.root(), b"/n/new"),
        Err(errno::ENOENT)
    ));
    assert_eq!(
        read(open(&ns, b"/m/dynamic", flags()), 99, true),
        b"dynamic:a\n"
    );
    assert_eq!(
        read(open(&ns, b"/n/dynamic", flags()), 99, true),
        b"dynamic:b\n"
    );
    let fd = open(&ns, b"/m/new", writable);
    let mut tx = Transaction::new(
        fd.clone(),
        Operation::Seek {
            current: 0,
            displacement: -2,
            whence: 2,
        },
        0,
    )
    .unwrap();
    assert!(matches!(complete(&mut tx), Ok(Reply::Position(6))));
    tx.commit();
    drop(tx);
    assert_eq!(read(fd.clone(), 99, true), b"12");
    let mut tx = Transaction::new(fd.clone(), Operation::Stat, 0).unwrap();
    assert!(matches!(
        complete(&mut tx),
        Ok(Reply::Stat(Stat { size: 8, .. }))
    ));
    drop(tx);
    let mut tx = Transaction::new(fd, Operation::Sync, 0).unwrap();
    assert!(matches!(complete(&mut tx), Ok(Reply::Done)));
    drop(tx);
    drop(open(
        &ns,
        b"/m/new",
        OpenFlags {
            truncate: true,
            ..writable
        },
    ));
    assert_eq!(read(open(&ns, b"/m/new", flags()), 99, true), b"");
}

#[test]
fn no_create_side_effect_for_invalid_directory_flags_or_overlong_canonical_path() {
    let (ns, a, _) = fixture();
    assert!(matches!(
        Open::new(
            ns.root(),
            b"/m/missing",
            OpenFlags {
                create: true,
                directory: true,
                ..flags()
            }
        ),
        Err(errno::EINVAL)
    ));
    assert_eq!(a.creates(), 0);
    let mut long = Vec::new();
    while long.len() < crate::config::GENRT_PATH_MAX - 2 {
        long.push(b'/');
        let length = (crate::config::GENRT_PATH_MAX - 2 - long.len()).min(NAME_MAX);
        long.resize(long.len() + length, b'a');
    }
    let mut ns = Namespace::new(root(vec![long.clone()])).unwrap();
    let a = Arc::new(TestFs::new("a"));
    ns.mount(&long, a.clone()).unwrap();
    let cwd = resolve(&ns, ns.root(), &long).unwrap();
    let mut op = Open::new(
        cwd,
        b"xx",
        OpenFlags {
            create: true,
            write: true,
            ..flags()
        },
    )
    .unwrap();
    assert!(matches!(
        op.poll(&ns, None),
        Poll::Ready(Err(errno::ENAMETOOLONG))
    ));
    assert_eq!(a.creates(), 0);
}

#[test]
fn bounded_names_reject_before_creation_and_remain_enumerable() {
    let (ns, a, _) = fixture();
    let writable = OpenFlags {
        create: true,
        write: true,
        ..flags()
    };
    let mut path = b"/m/".to_vec();
    path.resize(path.len() + NAME_MAX + 1, b'q');
    let mut op = Open::new(ns.root(), &path, writable).unwrap();
    assert!(matches!(
        op.poll(&ns, None),
        Poll::Ready(Err(errno::ENAMETOOLONG))
    ));
    assert_eq!(a.creates(), 0);
    path.pop();
    let file = open(&ns, &path, writable);
    write(file.clone(), b"boundary");
    assert_eq!(read(open(&ns, &path, flags()), 32, true), b"boundary");
    assert_eq!(a.creates(), 1);
    let directory = open(&ns, b"/m", flags());
    let mut found = false;
    for _ in 0..16 {
        let mut tx =
            Transaction::new(directory.clone(), Operation::Readdir { offset: 0 }, 0).unwrap();
        let Reply::Entry(entry) = complete(&mut tx).unwrap() else {
            panic!("not a directory entry");
        };
        let Some(entry) = entry else { break };
        assert!(entry.name.len() <= NAME_MAX);
        let mut encoded = [0; crate::memory::user::MAX_USER_COPY];
        assert!(
            crate::syscall::encode_dirent64(&mut encoded, entry.node.id, 1, 8, &entry.name)
                .is_some()
        );
        found |= entry.name == path[3..];
        tx.commit();
    }
    assert!(found);
}

#[test]
fn ramfs_validates_components_and_preserves_nonzero_inode_identity() {
    let mut path = vec![b'/'];
    path.resize(NAME_MAX + 2, b'x');
    assert!(matches!(
        MountedRamfs::new(
            vec![RamFile {
                path: path.clone(),
                data: b"x"
            }],
            vec![]
        ),
        Err(super::ramfs::MountError::InvalidPath)
    ));
    assert!(matches!(
        MountedRamfs::new(vec![], vec![path]),
        Err(super::ramfs::MountError::InvalidPath)
    ));
    let ns = Namespace::new(root(vec![])).unwrap();
    let file = resolve(&ns, ns.root(), b"/hello").unwrap();
    assert_eq!(file.node().id, 1);
    let mut tx = Transaction::new(
        open(&ns, b"/", flags()),
        Operation::Readdir { offset: 0 },
        0,
    )
    .unwrap();
    let Reply::Entry(Some(entry)) = complete(&mut tx).unwrap() else {
        panic!("missing archive entry");
    };
    assert_eq!(entry.name, b"hello");
    assert_eq!(entry.node, file.node());
}

#[test]
fn pending_transaction_retains_lease_and_zero_length_skips_backend() {
    let (ns, _, _) = fixture();
    let fd = open(&ns, b"/m/pending", flags());
    let control = open(
        &ns,
        b"/m/pending-control",
        OpenFlags {
            write: true,
            ..flags()
        },
    );
    let mut zero = Transaction::new(
        fd.clone(),
        Operation::Read {
            offset: 0,
            limit: 0,
        },
        0,
    )
    .unwrap();
    assert!(matches!(
        zero.poll(None),
        Poll::Ready(Ok(Reply::Io { count: 0, .. }))
    ));
    drop(zero);
    let cpu = crate::cpu::CpuId::from_index(0).unwrap();
    let token =
        crate::sched::WaitToken::for_profile_test(crate::sched::ThreadId::new(0, 1), cpu, 1);
    let mut tx = Transaction::new(
        fd.clone(),
        Operation::Read {
            offset: 0,
            limit: 99,
        },
        99,
    )
    .unwrap();
    assert!(tx.poll(Some(token)).is_pending());
    let mut peer = Transaction::new(
        fd,
        Operation::Read {
            offset: 0,
            limit: 99,
        },
        99,
    )
    .unwrap();
    assert!(matches!(peer.poll(None), Poll::Ready(Err(errno::EAGAIN))));
    for _ in 0..4 {
        assert!(tx.poll(Some(token)).is_pending());
    }
    write(control, b"1");
    assert!(matches!(
        tx.poll(Some(token)),
        Poll::Ready(Ok(Reply::Io { count: 10, .. }))
    ));
    assert_eq!(&tx.buffer()[..10], b"pending:a\n");
    tx.commit();
    drop(tx);
}

#[test]
fn fixed_fd_capacity_rejection_keeps_owned_description_for_unlocked_cleanup() {
    let (ns, _, _) = fixture();
    let mut fds = FdTable::new();
    for expected in 3..32 {
        assert_eq!(
            fds.install(&mut Some(open(&ns, b"/m/dynamic", flags()))),
            Ok(expected)
        );
    }
    assert_eq!(fds.next_free(), Err(errno::EMFILE));
    let mut staged = Some(open(&ns, b"/m/dynamic", flags()));
    assert_eq!(fds.install(&mut staged), Err(errno::EMFILE));
    assert!(staged.is_some());
    assert!(matches!(fds.get(0), Err(errno::EBADF)));
    drop(fds.take(7).unwrap());
    assert_eq!(fds.install(&mut staged), Ok(7));
    assert!(staged.is_none());
}

#[test]
fn final_reference_and_partial_open_failure_release_private_state_exactly_once() {
    let (ns, a, _) = fixture();
    let fd = open(&ns, b"/m/dynamic", flags());
    let alias = fd.clone();
    assert_eq!(a.counts(), (1, 0, 1));
    drop(fd);
    assert_eq!(a.counts(), (1, 0, 1));
    drop(alias);
    assert_eq!(a.counts(), (1, 1, 0));
    let mut failed = Open::new(ns.root(), b"/m/open-error", flags()).unwrap();
    assert!(matches!(
        failed.poll(&ns, None),
        Poll::Ready(Err(errno::EIO))
    ));
    drop(failed);
    assert_eq!(a.counts(), (2, 2, 0));
    let errors = open(
        &ns,
        b"/m/errors",
        OpenFlags {
            write: true,
            ..flags()
        },
    );
    let mut tx = Transaction::new(
        errors,
        Operation::Read {
            offset: 0,
            limit: 1,
        },
        1,
    )
    .unwrap();
    assert!(matches!(complete(&mut tx), Err(errno::EIO)));
    drop(tx);
    assert_eq!(a.counts(), (3, 3, 0));
}

#[test]
fn concurrent_distinct_append_descriptions_never_overwrite_or_split_records() {
    let (ns, _, _) = fixture();
    let writable = OpenFlags {
        read: true,
        write: true,
        create: true,
        append: true,
        ..OpenFlags::default()
    };
    let descriptions: Vec<_> = (0..4).map(|_| open(&ns, b"/m/append", writable)).collect();
    let handles: Vec<_> = descriptions
        .into_iter()
        .enumerate()
        .map(|(owner, fd)| {
            std::thread::spawn(move || {
                for _ in 0..64 {
                    let mut tx = Transaction::new(
                        fd.clone(),
                        Operation::Write {
                            offset: 0,
                            append: false,
                        },
                        8,
                    )
                    .unwrap();
                    assert!(matches!(
                        tx.prepare(None, |buffer| {
                            buffer.fill(owner as u8);
                            Ok(())
                        }),
                        Poll::Ready(Ok(()))
                    ));
                    // Synchronous host calls have no scheduler token. Only this
                    // test harness retries EAGAIN, yielding to the host OS.
                    loop {
                        match tx.poll(None) {
                            Poll::Ready(Ok(Reply::Io { count: 8, .. })) => {
                                tx.commit();
                                break;
                            }
                            Poll::Ready(Err(errno::EAGAIN)) => std::thread::yield_now(),
                            _ => panic!("unexpected concurrent append result"),
                        }
                    }
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let fd = open(&ns, b"/m/append", flags());
    let mut bytes = Vec::new();
    loop {
        let chunk = read(fd.clone(), crate::memory::user::MAX_USER_COPY, true);
        if chunk.is_empty() {
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    assert_eq!(bytes.len(), 2048);
    let mut counts = [0; 4];
    for chunk in bytes.chunks_exact(8) {
        assert!(chunk.iter().all(|b| *b == chunk[0]));
        counts[chunk[0] as usize] += 1;
    }
    assert_eq!(counts, [64; 4]);
}

#[test]
fn pending_create_open_truncate_and_write_resume_without_repeating_side_effects() {
    let backend = Arc::new(TestFs::new("deferred"));
    let mut ns = Namespace::new(root(vec![b"/m".to_vec()])).unwrap();
    ns.mount(b"/m", backend.clone()).unwrap();
    let cpu = crate::cpu::CpuId::from_index(0).unwrap();
    let token =
        crate::sched::WaitToken::for_profile_test(crate::sched::ThreadId::new(0, 1), cpu, 1);
    let mut operation = Open::new(
        ns.root(),
        b"/m/deferred-new",
        OpenFlags {
            read: true,
            write: true,
            create: true,
            truncate: true,
            ..OpenFlags::default()
        },
    )
    .unwrap();
    for _ in 0..3 {
        assert!(operation.poll(&ns, Some(token)).is_pending());
    }
    let Poll::Ready(Ok(description)) = operation.poll(&ns, Some(token)) else {
        panic!("open completion");
    };
    let mut input = *b"once";
    let (_, counts) = crate::test_arch_stubs::allocations(|| {
        let mut tx = Transaction::new(
            description.clone(),
            Operation::Write {
                offset: 0,
                append: false,
            },
            input.len(),
        )
        .unwrap();
        assert!(matches!(
            tx.prepare(Some(token), |buffer| {
                buffer.copy_from_slice(&input);
                Ok(())
            }),
            Poll::Ready(Ok(()))
        ));
        assert!(tx.poll(Some(token)).is_pending());
        let mut zero = write_transaction(&description, 0);
        assert!(matches!(
            zero.prepare(None, |_| panic!("zero write copied")),
            Poll::Ready(Ok(()))
        ));
        assert!(matches!(
            zero.poll(None),
            Poll::Ready(Ok(Reply::Io { count: 0, .. }))
        ));
        assert_eq!(zero.staging_identity(), None);
        let peer_token =
            crate::sched::WaitToken::for_profile_test(crate::sched::ThreadId::new(1, 1), cpu, 2);
        let mut peer = write_transaction(&description, 4);
        assert!(
            peer.prepare(Some(peer_token), |_| panic!("busy write copied"))
                .is_pending()
        );
        assert_eq!(peer.staging_identity(), None);
        assert_eq!(
            description.take_waiter_for_test(peer_token.thread().index()),
            Some(peer_token)
        );
        input.fill(b'x');
        assert!(matches!(
            tx.prepare(Some(token), |_| panic!("input copied twice")),
            Poll::Ready(Ok(()))
        ));
        assert!(matches!(
            tx.poll(Some(token)),
            Poll::Ready(Ok(Reply::Io { count: 4, next: 4 }))
        ));
        tx.commit();
    });
    assert_eq!(counts, (0, 0, 0));
    assert_eq!(backend.deferred_counts(), ([1; 4], [1; 4]));
    assert_eq!(backend.creates(), 1);
    let mut seek = Transaction::new(
        description.clone(),
        Operation::Seek {
            current: 0,
            displacement: 0,
            whence: 0,
        },
        0,
    )
    .unwrap();
    complete(&mut seek).unwrap();
    seek.commit();
    drop(seek);
    assert_eq!(read(description, 99, true), b"once");
}

fn read_transaction(fd: &Arc<super::fd::OpenDescription>, length: usize) -> Transaction {
    Transaction::new(
        fd.clone(),
        Operation::Read {
            offset: 0,
            limit: length,
        },
        length,
    )
    .unwrap()
}

fn write_transaction(fd: &Arc<super::fd::OpenDescription>, length: usize) -> Transaction {
    Transaction::new(
        fd.clone(),
        Operation::Write {
            offset: 0,
            append: false,
        },
        length,
    )
    .unwrap()
}

#[test]
fn first_and_repeated_small_io_reuse_request_without_any_heap_operations() {
    let (ns, _, _) = fixture();
    let fd = open(
        &ns,
        b"/m/reuse",
        OpenFlags {
            write: true,
            create: true,
            ..flags()
        },
    );
    let generated = open(&ns, b"/m/dynamic", flags());
    let stats = open(&ns, b"/m/stats", flags());
    let archive = open(&ns, b"/hello", flags());
    let errors = open(
        &ns,
        b"/m/errors",
        OpenFlags {
            write: true,
            ..flags()
        },
    );
    let (_, counts) = crate::test_arch_stubs::allocations(|| {
        let mut identity = None;
        for length in [1, 64, IO_CAPACITY, 7, IO_CAPACITY] {
            let mut write = write_transaction(&fd, length);
            assert!(matches!(
                write.prepare(None, |bytes| {
                    bytes.fill(0x5a);
                    Ok(())
                }),
                Poll::Ready(Ok(()))
            ));
            let current = write.staging_identity().unwrap();
            assert!(current.2 >= IO_CAPACITY);
            assert_eq!(*identity.get_or_insert(current), current);
            assert!(matches!(complete(&mut write), Ok(Reply::Io { count, .. }) if count == length));
            drop(write); // No offset commit: overwrite instead of growing backing.
            let mut read = read_transaction(&fd, length);
            assert!(matches!(complete(&mut read), Ok(Reply::Io { count, .. }) if count == length));
            assert_eq!(read.staging_identity(), identity);
            assert!(read.buffer().iter().all(|b| *b == 0x5a));
        }
        for _ in 0..3 {
            for (description, expected) in [
                (&generated, b"dynamic:a\n".as_slice()),
                (&archive, b"hello".as_slice()),
            ] {
                let mut tx = read_transaction(description, IO_CAPACITY);
                assert!(
                    matches!(complete(&mut tx), Ok(Reply::Io { count, .. }) if count == expected.len())
                );
                assert_eq!(&tx.buffer()[..expected.len()], expected);
            }
            let mut tx = read_transaction(&stats, IO_CAPACITY);
            assert!(matches!(complete(&mut tx), Ok(Reply::Io { count, .. }) if count > 0));
            drop(tx);
            let mut tx = read_transaction(&errors, 1);
            assert!(matches!(complete(&mut tx), Err(errno::EIO)));
            drop(tx);
            let mut tx = write_transaction(&errors, 1);
            assert!(matches!(
                tx.prepare(None, |bytes| {
                    bytes[0] = 1;
                    Ok(())
                }),
                Poll::Ready(Ok(()))
            ));
            assert!(matches!(complete(&mut tx), Err(errno::EIO)));
            assert!(matches!(
                Transaction::new(
                    archive.clone(),
                    Operation::Write {
                        offset: 0,
                        append: false
                    },
                    0
                ),
                Err(errno::EBADF)
            ));
        }
    });
    assert_eq!(counts, (0, 0, 0));
}

#[test]
fn write_fault_submits_nothing_and_short_reads_do_not_expose_old_staging() {
    let (ns, _, _) = fixture();
    let fd = open(
        &ns,
        b"/m/fault",
        OpenFlags {
            write: true,
            create: true,
            ..flags()
        },
    );
    let (_, counts) = crate::test_arch_stubs::allocations(|| {
        let mut tx = write_transaction(&fd, IO_CAPACITY);
        assert!(matches!(
            tx.prepare(None, |bytes| {
                bytes.fill(0xa5);
                Err(errno::EFAULT)
            }),
            Poll::Ready(Err(errno::EFAULT))
        ));
        drop(tx); // Failed syscall never submits a backend operation.
        let mut tx = read_transaction(&fd, IO_CAPACITY);
        assert!(matches!(
            complete(&mut tx),
            Ok(Reply::Io { count: 0, next: 0 })
        ));
        // Same-size reuse retains initialization instead of clearing 4 KiB.
        // EOF exposes none of those old staging bytes to the output copy.
        assert!(tx.buffer().iter().all(|b| *b == 0xa5));
        drop(tx);
        let mut tx = write_transaction(&fd, 3);
        assert!(matches!(
            tx.prepare(None, |bytes| {
                bytes.copy_from_slice(b"abc");
                Ok(())
            }),
            Poll::Ready(Ok(()))
        ));
        assert!(matches!(
            complete(&mut tx),
            Ok(Reply::Io { count: 3, next: 3 })
        ));
        drop(tx);
        for _ in 0..2 {
            let mut tx = read_transaction(&fd, IO_CAPACITY);
            let Reply::Io { count, next: 3 } = complete(&mut tx).unwrap() else {
                panic!("short read")
            };
            assert_eq!(count, 3);
            assert_eq!(&tx.buffer()[..3], b"abc");
            let mut output = [0xcc; 16];
            output[..count].copy_from_slice(&tx.buffer()[..count]);
            assert_eq!(&output[..3], b"abc");
            assert!(output[3..].iter().all(|b| *b == 0xcc));
            // Simulated failed read copy: drop without committing original offset.
        }
        let mut tx = read_transaction(&fd, 2);
        assert!(matches!(
            complete(&mut tx),
            Ok(Reply::Io { count: 2, next: 2 })
        ));
        tx.commit();
        drop(tx);
        let mut tx = read_transaction(&fd, IO_CAPACITY);
        assert!(matches!(
            complete(&mut tx),
            Ok(Reply::Io { count: 1, next: 3 })
        ));
        assert_eq!(tx.buffer()[0], b'c');
    });
    assert_eq!(counts, (0, 0, 0));
}

#[test]
fn large_io_grows_once_then_reuses_both_staging_and_backing_capacity() {
    let (ns, _, _) = fixture();
    let fd = open(
        &ns,
        b"/m/large",
        OpenFlags {
            write: true,
            create: true,
            ..flags()
        },
    );
    let (before, counts) = crate::test_arch_stubs::allocations(|| {
        let mut tx = read_transaction(&fd, 1);
        complete(&mut tx).unwrap();
        tx.staging_identity().unwrap()
    });
    assert_eq!(counts, (0, 0, 0));
    assert!(before.2 >= IO_CAPACITY);
    let length = IO_CAPACITY * 2 + 1;
    let (grown, counts) = crate::test_arch_stubs::allocations(|| {
        let mut tx = write_transaction(&fd, length);
        assert!(matches!(
            tx.prepare(None, |bytes| {
                bytes.fill(0x37);
                Ok(())
            }),
            Poll::Ready(Ok(()))
        ));
        assert!(matches!(complete(&mut tx), Ok(Reply::Io { count, .. }) if count == length));
        tx.staging_identity().unwrap()
    });
    assert_eq!(counts, (0, 2, 0)); // One retained staging and one backing growth.
    assert_eq!(before.0, grown.0);
    assert!(grown.2 >= length);
    let (_, counts) = crate::test_arch_stubs::allocations(|| {
        for size in [length, 1, IO_CAPACITY, length, 17] {
            let mut tx = read_transaction(&fd, size);
            assert!(matches!(complete(&mut tx), Ok(Reply::Io { count, .. }) if count == size));
            assert_eq!(tx.staging_identity(), Some(grown));
            assert!(tx.buffer().iter().all(|b| *b == 0x37));
            drop(tx);
            let mut tx = write_transaction(&fd, size);
            assert!(matches!(
                tx.prepare(None, |bytes| {
                    bytes.fill(0x37);
                    Ok(())
                }),
                Poll::Ready(Ok(()))
            ));
            complete(&mut tx).unwrap();
            assert_eq!(tx.staging_identity(), Some(grown));
        }
        // Deterministic capacity overflow must return the acquired lease on drop.
        let mut tx = read_transaction(&fd, usize::MAX);
        assert!(matches!(
            tx.prepare(None, |_| unreachable!()),
            Poll::Ready(Err(errno::ENOMEM))
        ));
        drop(tx);
        let mut tx = read_transaction(&fd, 1);
        complete(&mut tx).unwrap();
        assert_eq!(tx.staging_identity(), Some(grown));
    });
    assert_eq!(counts, (0, 0, 0));
}

#[test]
fn allocation_failure_on_open_closes_once_and_growth_failure_returns_the_lease() {
    let (ns, backend, _) = fixture();
    let mut opening = Open::new(ns.root(), b"/m/dynamic", flags()).unwrap();
    crate::test_arch_stubs::failing_allocation(IO_CAPACITY, || {
        assert!(matches!(
            opening.poll(&ns, None),
            Poll::Ready(Err(errno::ENOMEM))
        ));
    });
    assert_eq!(backend.counts(), (1, 0, 1));
    drop(opening);
    assert_eq!(backend.counts(), (1, 1, 0));
    let fd = open(&ns, b"/m/dynamic", flags());
    let mut tx = read_transaction(&fd, 1);
    complete(&mut tx).unwrap();
    let identity = tx.staging_identity();
    drop(tx);
    let (_, counts) = crate::test_arch_stubs::allocations(|| {
        crate::test_arch_stubs::failing_allocation(IO_CAPACITY + 1, || {
            let mut tx = read_transaction(&fd, IO_CAPACITY + 1);
            assert!(matches!(
                tx.prepare(None, |_| unreachable!()),
                Poll::Ready(Err(errno::ENOMEM))
            ));
            assert_eq!(tx.staging_identity(), identity);
        });
        let mut tx = read_transaction(&fd, 1);
        assert!(matches!(
            complete(&mut tx),
            Ok(Reply::Io { count: 1, next: 1 })
        ));
        assert_eq!(tx.staging_identity(), identity);
        assert_eq!(tx.buffer(), b"d");
    });
    assert_eq!(counts, (0, 1, 0));
    drop(fd);
    assert_eq!(backend.counts(), (2, 2, 0));
}

#[test]
fn pending_busy_zero_and_abandoned_io_reuse_the_same_exclusive_request() {
    let (ns, a, _) = fixture();
    let control = open(
        &ns,
        b"/m/pending-control",
        OpenFlags {
            write: true,
            ..flags()
        },
    );
    let mut parent = FdTable::new();
    parent
        .install(&mut Some(open(&ns, b"/m/pending", flags())))
        .unwrap();
    let mut child = parent.clone();
    let fd = parent.get(3).unwrap();
    let cpu = crate::cpu::CpuId::from_index(0).unwrap();
    let token =
        crate::sched::WaitToken::for_profile_test(crate::sched::ThreadId::new(0, 1), cpu, 1);
    let peer_token =
        crate::sched::WaitToken::for_profile_test(crate::sched::ThreadId::new(1, 1), cpu, 2);
    let (_, counts) = crate::test_arch_stubs::allocations(|| {
        let mut tx = read_transaction(&fd, 5);
        assert!(tx.poll(Some(token)).is_pending());
        let identity = tx.staging_identity();
        let mut peer = read_transaction(&child.get(3).unwrap(), 5);
        assert!(
            peer.prepare(Some(peer_token), |_| panic!("busy request touched"))
                .is_pending()
        );
        assert_eq!(peer.staging_identity(), None);
        assert_eq!(
            fd.take_waiter_for_test(peer_token.thread().index()),
            Some(peer_token)
        );
        assert!(matches!(peer.poll(None), Poll::Ready(Err(errno::EAGAIN))));
        let mut zero = read_transaction(&fd, 0);
        assert!(matches!(
            zero.prepare(None, |_| panic!("zero copied")),
            Poll::Ready(Ok(()))
        ));
        assert!(matches!(
            zero.poll(None),
            Poll::Ready(Ok(Reply::Io { count: 0, .. }))
        ));
        assert_eq!(zero.staging_identity(), None);
        write(control.clone(), b"1");
        assert!(matches!(
            tx.poll(Some(token)),
            Poll::Ready(Ok(Reply::Io { count: 5, next: 5 }))
        ));
        assert_eq!(tx.buffer(), b"pendi");
        tx.commit();
        drop(tx);
        assert!(matches!(
            peer.poll(Some(peer_token)),
            Poll::Ready(Ok(Reply::Io { count: 5, next: 10 }))
        ));
        assert_eq!(peer.staging_identity(), identity);
        assert_eq!(peer.buffer(), b"ng:a\n");
        drop(peer); // No commit: retain offset 5 after failed read usercopy.
        write(control.clone(), b"0");
        let mut abandoned = read_transaction(&fd, 5);
        assert!(abandoned.poll(Some(token)).is_pending());
        drop(abandoned);
        let mut retry = read_transaction(&fd, 5);
        assert!(retry.poll(Some(token)).is_pending()); // Fresh operation, not stale marker.
        write(control.clone(), b"1");
        assert!(matches!(
            retry.poll(Some(token)),
            Poll::Ready(Ok(Reply::Io { count: 5, next: 10 }))
        ));
        assert_eq!(retry.buffer(), b"ng:a\n");
        assert_eq!(retry.staging_identity(), identity);
    });
    assert_eq!(counts, (0, 0, 0));
    drop(control);
    drop(parent.take(3).unwrap());
    drop(child.take(3).unwrap());
    assert_eq!(a.counts(), (2, 1, 1));
    drop(fd);
    assert_eq!(a.counts(), (2, 2, 0));
}

#[test]
fn pending_gate_validates_controls_and_cleans_up_replaced_waits() {
    let (ns, _, _) = fixture();
    let control = open(
        &ns,
        b"/m/pending-control",
        OpenFlags {
            write: true,
            ..flags()
        },
    );
    let fd = open(&ns, b"/m/pending", flags());
    let cpu = crate::cpu::CpuId::from_index(0).unwrap();
    let thread = crate::sched::ThreadId::new(0, 1);
    let token = crate::sched::WaitToken::for_profile_test(thread, cpu, 1);
    let replacement = crate::sched::WaitToken::for_profile_test(thread, cpu, 2);
    let mut tx = read_transaction(&fd, 5);
    assert!(tx.poll(Some(token)).is_pending());
    assert!(tx.poll(Some(replacement)).is_pending());
    let mut hold = Transaction::new(
        control.clone(),
        Operation::Write {
            offset: 0,
            append: false,
        },
        1,
    )
    .unwrap();
    assert!(matches!(
        hold.prepare(None, |buffer| {
            buffer.copy_from_slice(b"0");
            Ok(())
        }),
        Poll::Ready(Ok(()))
    ));
    assert!(matches!(complete(&mut hold), Err(errno::EBUSY)));
    drop(hold);
    let mut invalid = Transaction::new(
        control.clone(),
        Operation::Write {
            offset: 0,
            append: false,
        },
        1,
    )
    .unwrap();
    assert!(matches!(
        invalid.prepare(None, |buffer| {
            buffer.copy_from_slice(b"x");
            Ok(())
        }),
        Poll::Ready(Ok(()))
    ));
    assert!(matches!(complete(&mut invalid), Err(errno::EINVAL)));
    drop(invalid);
    assert!(tx.poll(Some(replacement)).is_pending());
    drop(tx);
    // Abandonment must detach the latest exact token, not only the first one.
    write(control.clone(), b"0");
    // Release before submission must not require an artificial timer wake.
    write(control.clone(), b"1");
    assert_eq!(read(fd.clone(), 5, true), b"pendi");
    write(control.clone(), b"1");
    assert_eq!(read(fd, 5, true), b"ng:a\n");
}
