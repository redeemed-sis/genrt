//! Persistent-mount type probes, separate from production ownership.
//!
//! An owning filesystem can be moved when its sector owner is `Send`:
//!
//! ```
//! use genrt_fat_poc_issue24::{Sectors, retained::System};
//! fn assert_send<T: Send>() {}
//! fn check<D: Sectors + Send>() { assert_send::<System<D>>(); }
//! ```
//!
//! Its borrowed handles cannot satisfy the current OFD's `Any + Send` bound,
//! even with a genuinely `Send` sector owner. `RefCell` in the filesystem makes
//! the shared filesystem reference non-`Send`:
//!
//! ```compile_fail,E0277
//! use genrt_fat_poc_issue24::{Sectors, retained::File};
//! fn assert_send<T: Send>() {}
//! fn check<D: Sectors + Send + 'static>() {
//!     assert_send::<File<'static, D>>();
//! }
//! ```
//!
//! Erasing only the `Send` bound does not make a borrowed file `Any`/`'static`:
//!
//! ```compile_fail
//! use genrt_fat_poc_issue24::{Sectors, retained::File};
//! fn into_ofd<'fs, D: Sectors + 'static>(file: File<'fs, D>)
//!     -> std::boxed::Box<dyn core::any::Any> {
//!     std::boxed::Box::new(file)
//! }
//! ```
//!
//! A file cannot escape the temporary filesystem used by today's adapter:
//!
//! ```compile_fail,E0515
//! use genrt_fat_poc_issue24::{Cursor, Sectors, retained::{File, System}};
//! fn escape<D: Sectors + 'static>(device: D) -> File<'static, D> {
//!     let fs = System::new(Cursor::new(device), fatfs::FsOptions::new()).unwrap();
//!     fs.root_dir().open_file("EXISTING.BIN").unwrap()
//! }
//! ```
//!
//! Consuming unmount cannot publish metadata while a handle is still needed:
//!
//! ```compile_fail,E0505
//! use genrt_fat_poc_issue24::{Sectors, retained::System};
//! fn sync_live<D: Sectors>(fs: System<D>) {
//!     let file = fs.root_dir().open_file("EXISTING.BIN").unwrap();
//!     fs.unmount().unwrap();
//!     let _ = file.size();
//! }
//! ```

use crate::Cursor;

/// Library filesystem owning its cursor; no borrowed kernel `Disk` is retained.
pub type System<D> = fatfs::FileSystem<Cursor<D>>;

/// Library file borrowing that filesystem, not a standalone owned descriptor.
pub type File<'fs, D> =
    fatfs::File<'fs, Cursor<D>, fatfs::DefaultTimeProvider, fatfs::LossyOemCpConverter>;
