use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::iter::FromIterator;
use std::os::raw::c_int;
use std::path::Path;

use bimap::BiHashMap;
use fuse::{FileAttr, Filesystem, FileType, ReplyAttr, ReplyBmap, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyLock, ReplyOpen, ReplyStatfs, ReplyWrite, ReplyXattr, Request};
use json_flex::JFObject;
use libc::ENOSYS;
use log::info;
use reqwest::blocking::Client;
use time::Timespec;

use crate::github_filesystem::cache::LRUCache;
use crate::github_filesystem::http::json_request;

pub(crate) mod http;
mod cache;

pub(crate) fn mount(url: &str, client: Client, mount_point: &str, username: String, password: String) {
    let mut files = HashMap::new();
    files.insert(1, create_github_directory(1, 0, 0, 0, OsString::new(), url.to_string()));
    let mut children = HashMap::new();
    children.insert(1, HashSet::new());

    fuse::mount(GitHubFS {
        files,
        children,
        cache: LRUCache::new(),
        inode_max: 1,
        client,
        username,
        password,
    }, &mount_point, &[]).expect("fail mount()");
}

fn create_empty_file(ino: u64, parent: u64, uid: u32, gid: u32, name: OsString) -> FileData {
    let t = time::now().to_timespec();
    FileData {
        parent,
        attr: FileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: t,
            mtime: t,
            ctime: t,
            crtime: t,
            kind: FileType::RegularFile,
            perm: 0o664,
            nlink: 0,
            uid,
            gid,
            rdev: 0,
            flags: 0,
        },
        name,
        data: GitHubFileData::Raw(Vec::new()),
        lookup_count: 0,
        removed: false,
    }
}

fn create_directory(ino: u64, parent: u64, uid: u32, gid: u32, name: OsString) -> FileData {
    let t = time::now().to_timespec();
    FileData {
        parent,
        attr: FileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: t,
            mtime: t,
            ctime: t,
            crtime: t,
            kind: FileType::Directory,
            perm: 0o775,
            nlink: 0,
            uid,
            gid,
            rdev: 0,
            flags: 0,
        },
        name,
        data: GitHubFileData::None,
        lookup_count: 0,
        removed: false,
    }
}

fn create_github_file(ino: u64, size: u64, parent: u64, uid: u32, gid: u32, name: OsString, url: String) -> FileData {
    let t = time::now().to_timespec();
    FileData {
        parent,
        attr: FileAttr {
            ino,
            size,
            blocks: 0,
            atime: t,
            mtime: t,
            ctime: t,
            crtime: t,
            kind: FileType::RegularFile,
            perm: 0o444,
            nlink: 0,
            uid,
            gid,
            rdev: 0,
            flags: 0,
        },
        name,
        data: GitHubFileData::GitHubFile { url },
        lookup_count: 0,
        removed: false,
    }
}

fn create_github_directory(ino: u64, parent: u64, uid: u32, gid: u32, name: OsString, url: String) -> FileData {
    let t = time::now().to_timespec();
    FileData {
        parent,
        attr: FileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: t,
            mtime: t,
            ctime: t,
            crtime: t,
            kind: FileType::Directory,
            perm: 0o555,
            nlink: 0,
            uid,
            gid,
            rdev: 0,
            flags: 0,
        },
        name,
        data: GitHubFileData::GitHubDirectory { url, fetched: false },
        lookup_count: 0,
        removed: false,
    }
}

fn update_atime(file: &mut FileAttr) {
    file.atime = time::now().to_timespec();
}

fn update_mtime(file: &mut FileAttr) {
    file.mtime = time::now().to_timespec();
}

fn update_ctime(file: &mut FileAttr) {
    file.ctime = time::now().to_timespec();
}


enum GitHubFileData {
    Raw(Vec<u8>),
    GitHubFile { url: String },
    GitHubDirectory { url: String, fetched: bool },
    None,
}

struct FileData {
    parent: u64,
    attr: FileAttr,
    name: OsString,
    data: GitHubFileData,
    lookup_count: usize,
    removed: bool,
}

struct GitHubFS {
    files: HashMap<u64, FileData>,
    children: HashMap<u64, HashSet<u64>>,
    cache: LRUCache<Vec<u8>>,
    inode_max: u64,
    client: Client,
    username: String,
    password: String,
}

const APP_NAME: &str = "ghfs";

impl GitHubFS {
    fn load_directory_if_not_loaded(&mut self, uid: u32, gid: u32, parent: u64) {
        if let Some(FileData { data: GitHubFileData::GitHubDirectory { url, fetched: false }, .. }) = self.files.get(&parent) {
            let url = url.clone();
            let files = self.load_child_files_from_github(&url, parent, uid, gid);
            if let Some(FileData { data: GitHubFileData::GitHubDirectory { fetched, .. }, .. }) = self.files.get_mut(&parent) {
                *fetched = true;
            } else { unreachable!(); }
            for file in files {
                self.insert_file(file, parent);
            }
        }
    }

    fn get_file_data(&mut self, ino: u64) -> Option<&Vec<u8>> {
        let file = match self.files.get(&ino) {
            None => return None,
            Some(file) => file,
        };
        match &file.data {
            GitHubFileData::GitHubFile { url } => {
                if self.cache.contains(ino) {
                    self.cache.get_cache(ino)
                } else {
                    Some(self.cache.insert_cache(ino, self.load_file_from_github(url)))
                }
            }
            GitHubFileData::Raw(vec) => Some(vec),
            _ => { None }
        }
    }

    fn insert_file(&mut self, file: FileData, parent: u64) {
        if let Some(children) = self.children.get_mut(&parent) {
            children.insert(file.attr.ino);
        } else {
            let mut children = HashSet::with_capacity(1);
            children.insert(file.attr.ino);
            self.children.insert(parent, children);
        }
        self.files.insert(file.attr.ino, file);
    }

    fn get_json(&self, url: &str) -> Box<JFObject> {
        json_request(&self.client, url, &self.username, &self.password)
    }

    fn load_file_from_github(&self, url: &str) -> Vec<u8> {
        let json = self.get_json(url);
        base64::decode(&json["content"].unwrap_string().replace("\\n", "")).unwrap()
    }

    fn load_child_files_from_github(&mut self, url: &str, parent: u64, uid: u32, gid: u32) -> Vec<FileData> {
        let json = self.get_json(url);
        json["tree"].unwrap_vec().iter().map(|file| {
            match file["type"].unwrap_string().as_str() {
                "blob" => {
                    create_github_file(
                        self.next_ino(),
                        *file["size"].unwrap_i64() as u64,
                        parent,
                        uid,
                        gid,
                        OsString::from(file["path"].unwrap_string()),
                        file["url"].unwrap_string().clone())
                }
                "tree" => {
                    create_github_directory(
                        self.next_ino(),
                        parent,
                        uid,
                        gid,
                        OsString::from(file["path"].unwrap_string()),
                        file["url"].unwrap_string().clone())
                }
                _ => unimplemented!()
            }
        }).collect()
    }

    fn next_ino(&mut self) -> u64 {
        self.inode_max += 1;
        self.inode_max
    }
}

impl Filesystem for GitHubFS {
    /// Initialize filesystem.
    /// Called before any other filesystem method.
    fn init(&mut self, _req: &Request) -> Result<(), c_int> {
        info!("いにしゃらいず");
        Ok(())
    }

    /// Clean up filesystem.
    /// Called on filesystem exit.
    fn destroy(&mut self, _req: &Request) {
        info!("ですとろーい");
    }

    /// Look up a directory entry by name and get its attributes.
    fn lookup(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if parent == 1 && name == ".." {
            reply.entry(&Timespec::new(1, 0), &self.files[&1].attr, 0);
            return;
        }
        self.load_directory_if_not_loaded(req.uid(), req.gid(), parent);
        if let Some(vec) = self.children.get(&parent) {
            for inode in vec {
                if let Some(file) = self.files.get_mut(inode) {
                    if name == file.name {
                        file.lookup_count += 1;
                        reply.entry(&Timespec::new(0, 0), &file.attr, 0);
                        return;
                    }
                }
            }
        }
        reply.error(libc::ENOENT);
    }

    /// Forget about an inode.
    /// The nlookup parameter indicates the number of lookups previously performed on
    /// this inode. If the filesystem implements inode lifetimes, it is recommended that
    /// inodes acquire a single reference on each lookup, and lose nlookup references on
    /// each forget. The filesystem may ignore forget calls, if the inodes don't need to
    /// have a limited lifetime. On unmount it is not guaranteed, that all referenced
    /// inodes will receive a forget message.
    fn forget(&mut self, _req: &Request, _ino: u64, _nlookup: u64) {}

    /// Get file attributes.
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        match self.files.get(&ino) {
            Some(file) => {
                reply.attr(&Timespec::new(1, 0), &file.attr)
            }
            None => reply.error(ENOSYS),
        }
    }

    /// Set file attributes.
    fn setattr(&mut self, _req: &Request, ino: u64, mode: Option<u32>, uid: Option<u32>, gid: Option<u32>, size: Option<u64>, atime: Option<Timespec>, mtime: Option<Timespec>, _fh: Option<u64>, crtime: Option<Timespec>, _chgtime: Option<Timespec>, _bkuptime: Option<Timespec>, flags: Option<u32>, reply: ReplyAttr) {
        //TODO:permission
        if let Some(file) = self.files.get_mut(&ino) {
            if let Some(n) = mode { file.attr.perm = n as u16; }
            if let Some(n) = uid { file.attr.uid = n; }
            if let Some(n) = gid { file.attr.gid = n; }
            if let Some(n) = size {
                if n != file.attr.size {
                    match &mut file.data {
                        GitHubFileData::Raw(vec) => {
                            file.attr.size = n;
                            vec.resize(n as usize, 0);
                            update_mtime(&mut file.attr);
                        }
                        _ => {}
                    }
                }
            };
            if let Some(n) = atime { file.attr.atime = n; }
            if let Some(n) = mtime { file.attr.mtime = n; }
            if let Some(n) = crtime { file.attr.crtime = n; }
            if let Some(n) = flags { file.attr.flags = n; }
            update_ctime(&mut file.attr);
            reply.attr(&Timespec::new(1, 0), &file.attr);
        } else {
            reply.error(ENOSYS);
        }
    }

    /// Read symbolic link.
    fn readlink(&mut self, _req: &Request, _ino: u64, reply: ReplyData) {
        reply.error(ENOSYS);
    }

    /// Create file node.
    /// Create a regular file, character device, block device, fifo or socket node.
    fn mknod(&mut self, _req: &Request, _parent: u64, _name: &OsStr, _mode: u32, _rdev: u32, reply: ReplyEntry) {
        reply.error(ENOSYS);
    }

    /// Create a directory.
    fn mkdir(&mut self, req: &Request, parent: u64, name: &OsStr, _mode: u32, reply: ReplyEntry) {
        let ino = self.next_ino();
        let file = create_directory(ino, parent, req.uid(), req.gid(), name.to_os_string());
        reply.entry(&Timespec::new(1, 0), &file.attr, 0);
        self.files.insert(ino, file);
        if let Some(parent) = self.files.get_mut(&parent) {
            update_mtime(&mut parent.attr);
            update_ctime(&mut parent.attr);
        }
        match self.children.get_mut(&parent) {
            Some(child) => { child.insert(ino); }
            None => { self.children.insert(parent, HashSet::from_iter(vec![ino])); }
        }
    }

    /// Remove a file.
    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        //TODO:permission
        let name = name.to_os_string();
        if let Some(child) = self.children.get(&parent) {
            for ino in child {
                if let Some(file) = self.files.get(ino) {
                    if file.name == name {
                        match file.data {
                            GitHubFileData::Raw(_) | GitHubFileData::None => {}
                            _ => {
                                reply.error(ENOSYS);
                                return;
                            }
                        }
                        if file.attr.kind == FileType::RegularFile {
                            self.files.remove(ino);
                            if let Some(parent) = self.files.get_mut(&parent) {
                                update_mtime(&mut parent.attr);
                                update_ctime(&mut parent.attr);
                            }
                            reply.ok();
                        } else {
                            reply.error(ENOSYS);
                        }
                        return;
                    }
                }
            }
        }
        reply.error(ENOSYS);
    }

    /// Remove a directory.
    fn rmdir(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        //TODO:permission
        let name = name.to_os_string();
        if let Some(child) = self.children.get(&parent) {
            for ino in child {
                if let Some(file) = self.files.get(ino) {
                    if file.name == name {
                        match file.data {
                            GitHubFileData::None => {}
                            _ => {
                                reply.error(ENOSYS);
                                return;
                            }
                        }
                        if file.attr.kind == FileType::Directory {
                            self.files.remove(ino);
                            if let Some(parent) = self.files.get_mut(&parent) {
                                update_mtime(&mut parent.attr);
                                update_ctime(&mut parent.attr);
                            }
                            reply.ok();
                        } else {
                            reply.error(ENOSYS);
                        }
                        return;
                    }
                }
            }
        }
        reply.error(ENOSYS);
    }

    /// Create a symbolic link.
    fn symlink(&mut self, _req: &Request, _parent: u64, _name: &OsStr, _link: &Path, reply: ReplyEntry) {
        reply.error(ENOSYS);
    }

    /// Rename a file.
    fn rename(&mut self, _req: &Request, parent: u64, name: &OsStr, newparent: u64, newname: &OsStr, reply: ReplyEmpty) {
        //TODO:permission
        let name = name.to_os_string();
        let newname = newname.to_os_string();
        match self.children.get(&parent) {
            None => {
                reply.error(ENOSYS);
                return;
            }
            Some(child) => {
                let ino = child.iter()
                    .find_map(|ino|
                        if let Some(FileData { name: filename, data, .. }) = self.files.get(ino) {
                            match data {
                                GitHubFileData::Raw(_) | GitHubFileData::None => {
                                    if filename == &name {
                                        Some(*ino)
                                    } else { None }
                                }
                                _ => { None }
                            }
                        } else { None });
                if let Some(ino) = ino {
                    if let Some(parent) = self.files.get_mut(&parent) {
                        update_mtime(&mut parent.attr);
                        update_ctime(&mut parent.attr);
                    }
                    if let Some(parent) = self.files.get_mut(&newparent) {
                        update_mtime(&mut parent.attr);
                        update_ctime(&mut parent.attr);
                    }
                    let file = self.files.get_mut(&ino).unwrap();
                    update_ctime(&mut file.attr);
                    file.parent = newparent;
                    file.name = newname;
                    if let Some(child) = self.children.get_mut(&parent) {
                        child.remove(&ino);
                    }
                    match self.children.get_mut(&newparent) {
                        Some(child) => { child.insert(ino); }
                        None => { self.children.insert(newparent, HashSet::from_iter(vec![ino])); }
                    }
                    reply.ok();
                    return;
                } else {
                    reply.error(ENOSYS);
                    return;
                }
            }
        }
    }

    /// Create a hard link.
    fn link(&mut self, _req: &Request, _ino: u64, _newparent: u64, _newname: &OsStr, reply: ReplyEntry) {
        reply.error(ENOSYS);
    }

    /// Open a file.
    /// Open flags (with the exception of O_CREAT, O_EXCL, O_NOCTTY and O_TRUNC) are
    /// available in flags. Filesystem may store an arbitrary file handle (pointer, index,
    /// etc) in fh, and use this in other all other file operations (read, write, flush,
    /// release, fsync). Filesystem may also implement stateless file I/O and not store
    /// anything in fh. There are also some flags (direct_io, keep_cache) which the
    /// filesystem may set, to change the way the file is opened. See fuse_file_info
    /// structure in <fuse_common.h> for more details.
    fn open(&mut self, _req: &Request, _ino: u64, flags: u32, reply: ReplyOpen) {
        reply.opened(0, flags);
    }

    /// Read data.
    /// Read should send exactly the number of bytes requested except on EOF or error,
    /// otherwise the rest of the data will be substituted with zeroes. An exception to
    /// this is when the file has been opened in 'direct_io' mode, in which case the
    /// return value of the read system call will reflect the return value of this
    /// operation. fh will contain the value set by the open method, or will be undefined
    /// if the open method didn't set any value.
    fn read(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, size: u32, reply: ReplyData) {
        //TODO:permission
        let offset = offset as usize;
        let size = size as usize;
        match self.get_file_data(ino) {
            Some(vec) => {
                // update_atime(attr);
                if offset < vec.len() {
                    reply.data(&vec[offset..(offset + size).min(vec.len())]);
                } else {
                    reply.data(&[]);
                }
            }
            _ => reply.error(ENOSYS)
        }
    }

    /// Write data.
    /// Write should return exactly the number of bytes requested except on error. An
    /// exception to this is when the file has been opened in 'direct_io' mode, in
    /// which case the return value of the write system call will reflect the return
    /// value of this operation. fh will contain the value set by the open method, or
    /// will be undefined if the open method didn't set any value.
    fn write(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, data: &[u8], _flags: u32, reply: ReplyWrite) {
        //TODO:permission
        match self.files.get_mut(&ino) {
            Some(FileData { attr, data: GitHubFileData::Raw(vec), .. }) => {
                update_mtime(attr);
                update_ctime(attr);
                if vec.len() < offset as usize + data.len() {
                    vec.resize(offset as usize + data.len(), 0);
                    attr.size = vec.len() as u64;
                }
                vec[offset as usize..offset as usize + data.len()].clone_from_slice(data);
                reply.written(data.len() as u32);
            }
            _ => reply.error(ENOSYS),
        }
    }

    /// Flush method.
    /// This is called on each close() of the opened file. Since file descriptors can
    /// be duplicated (dup, dup2, fork), for one open call there may be many flush
    /// calls. Filesystems shouldn't assume that flush will always be called after some
    /// writes, or that if will be called at all. fh will contain the value set by the
    /// open method, or will be undefined if the open method didn't set any value.
    /// NOTE: the name of the method is misleading, since (unlike fsync) the filesystem
    /// is not forced to flush pending writes. One reason to flush data, is if the
    /// filesystem wants to return write errors. If the filesystem supports file locking
    /// operations (setlk, getlk) it should remove all locks belonging to 'lock_owner'.
    fn flush(&mut self, _req: &Request, _ino: u64, _fh: u64, _lock_owner: u64, reply: ReplyEmpty) {
        reply.ok();
    }

    /// Release an open file.
    /// Release is called when there are no more references to an open file: all file
    /// descriptors are closed and all memory mappings are unmapped. For every open
    /// call there will be exactly one release call. The filesystem may reply with an
    /// error, but error values are not returned to close() or munmap() which triggered
    /// the release. fh will contain the value set by the open method, or will be undefined
    /// if the open method didn't set any value. flags will contain the same flags as for
    /// open.
    fn release(&mut self, _req: &Request, _ino: u64, _fh: u64, _flags: u32, _lock_owner: u64, _flush: bool, reply: ReplyEmpty) {
        // //実際の削除はここでやる
        // if let Some(&FileData { removed: true, .. }) = self.files.get(&ino) {
        //     self.files.remove(&ino);
        // }
        reply.ok();
    }

    /// Synchronize file contents.
    /// If the datasync parameter is non-zero, then only the user data should be flushed,
    /// not the meta data.
    fn fsync(&mut self, _req: &Request, _ino: u64, _fh: u64, _datasync: bool, reply: ReplyEmpty) {
        reply.error(ENOSYS);
    }

    /// Open a directory.
    /// Filesystem may store an arbitrary file handle (pointer, index, etc) in fh, and
    /// use this in other all other directory stream operations (readdir, releasedir,
    /// fsyncdir). Filesystem may also implement stateless directory I/O and not store
    /// anything in fh, though that makes it impossible to implement standard conforming
    /// directory stream operations in case the contents of the directory can change
    /// between opendir and releasedir.
    fn opendir(&mut self, _req: &Request, _ino: u64, _flags: u32, reply: ReplyOpen) {
        reply.opened(0, _flags);
    }

    /// Read directory.
    /// Send a buffer filled using buffer.fill(), with size not exceeding the
    /// requested size. Send an empty buffer on end of stream. fh will contain the
    /// value set by the opendir method, or will be undefined if the opendir method
    /// didn't set any value.
    fn readdir(&mut self, req: &Request, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
        self.load_directory_if_not_loaded(req.uid(), req.gid(), ino);
        let offset = offset as usize;
        let current = OsString::from(".");
        let parent = OsString::from("..");
        if let Some(file) = self.files.get_mut(&ino) {
            update_atime(&mut file.attr);
        }
        let iter = self.files.get(&ino)
            .map_or(
                Vec::new(),
                |file|
                    vec![
                        Some((&file.attr, &current)),
                        if ino == 1 {
                            Some((&file.attr, &parent))
                        } else {
                            self.files.get(&file.parent).map_or(None, |file| Some((&file.attr, &parent)))
                        }]).into_iter()
            .flatten()
            .chain(
                self.children.get(&ino)
                    .into_iter()
                    .flatten()
                    .filter_map(|ino| {
                        self.files.get(ino)
                            .map(|file| (&file.attr, &file.name))
                    })
            );
        for (i, (f, n)) in iter.enumerate().skip(offset) {
            if reply.add(f.ino, i as i64 + 1, f.kind, n) {
                break;
            }
        }
        reply.ok();
    }

    /// Release an open directory.
    /// For every opendir call there will be exactly one releasedir call. fh will
    /// contain the value set by the opendir method, or will be undefined if the
    /// opendir method didn't set any value.
    fn releasedir(&mut self, _req: &Request, _ino: u64, _fh: u64, _flags: u32, reply: ReplyEmpty) {
        // if let Some(&FileData { removed: true, .. }) = self.files.get(&ino) {
        //     self.files.remove(&ino);
        // }
        reply.ok();
    }

    /// Synchronize directory contents.
    /// If the datasync parameter is set, then only the directory contents should
    /// be flushed, not the meta data. fh will contain the value set by the opendir
    /// method, or will be undefined if the opendir method didn't set any value.
    fn fsyncdir(&mut self, _req: &Request, _ino: u64, _fh: u64, _datasync: bool, reply: ReplyEmpty) {
        reply.error(ENOSYS);
    }

    /// Get file system statistics.
    fn statfs(&mut self, _req: &Request, _ino: u64, reply: ReplyStatfs) {
        reply.statfs(0, 0, 0, 0, 0, 512, 255, 0);
    }

    /// Set an extended attribute.
    fn setxattr(&mut self, _req: &Request, _ino: u64, _name: &OsStr, _value: &[u8], _flags: u32, _position: u32, reply: ReplyEmpty) {
        reply.error(ENOSYS);
    }

    /// Get an extended attribute.
    /// If `size` is 0, the size of the value should be sent with `reply.size()`.
    /// If `size` is not 0, and the value fits, send it with `reply.data()`, or
    /// `reply.error(ERANGE)` if it doesn't.
    fn getxattr(&mut self, _req: &Request, _ino: u64, _name: &OsStr, _size: u32, reply: ReplyXattr) {
        reply.error(ENOSYS);
    }

    /// List extended attribute names.
    /// If `size` is 0, the size of the value should be sent with `reply.size()`.
    /// If `size` is not 0, and the value fits, send it with `reply.data()`, or
    /// `reply.error(ERANGE)` if it doesn't.
    fn listxattr(&mut self, _req: &Request, _ino: u64, _size: u32, reply: ReplyXattr) {
        reply.error(ENOSYS);
    }

    /// Remove an extended attribute.
    fn removexattr(&mut self, _req: &Request, _ino: u64, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(ENOSYS);
    }

    /// Check file access permissions.
    /// This will be called for the access() system call. If the 'default_permissions'
    /// mount option is given, this method is not called. This method is not called
    /// under Linux kernel versions 2.4.x
    fn access(&mut self, _req: &Request, _ino: u64, _mask: u32, reply: ReplyEmpty) {
        reply.ok();
        // reply.error(ENOSYS);
    }

    /// Create and open a file.
    /// If the file does not exist, first create it with the specified mode, and then
    /// open it. Open flags (with the exception of O_NOCTTY) are available in flags.
    /// Filesystem may store an arbitrary file handle (pointer, index, etc) in fh,
    /// and use this in other all other file operations (read, write, flush, release,
    /// fsync). There are also some flags (direct_io, keep_cache) which the
    /// filesystem may set, to change the way the file is opened. See fuse_file_info
    /// structure in <fuse_common.h> for more details. If this method is not
    /// implemented or under Linux kernel versions earlier than 2.6.15, the mknod()
    /// and open() methods will be called instead.
    fn create(&mut self, req: &Request, parent: u64, name: &OsStr, _mode: u32, flags: u32, reply: ReplyCreate) {
        //TODO:permission
        let ino = self.next_ino();
        let file = create_empty_file(ino, parent, req.uid(), req.gid(), name.to_os_string());
        reply.created(&Timespec::new(1, 0), &file.attr, 0, 0, flags);
        self.files.insert(ino, file);
        if let Some(parent) = self.files.get_mut(&parent) {
            update_mtime(&mut parent.attr);
            update_ctime(&mut parent.attr);
        }
        match self.children.get_mut(&parent) {
            Some(child) => { child.insert(ino); }
            None => { self.children.insert(parent, HashSet::from_iter(vec![ino])); }
        }
    }

    /// Test for a POSIX file lock.
    fn getlk(&mut self, _req: &Request, _ino: u64, _fh: u64, _lock_owner: u64, _start: u64, _end: u64, _typ: u32, _pid: u32, reply: ReplyLock) {
        reply.error(ENOSYS);
    }

    /// Acquire, modify or release a POSIX file lock.
    /// For POSIX threads (NPTL) there's a 1-1 relation between pid and owner, but
    /// otherwise this is not always the case.  For checking lock ownership,
    /// 'fi->owner' must be used. The l_pid field in 'struct flock' should only be
    /// used to fill in this field in getlk(). Note: if the locking methods are not
    /// implemented, the kernel will still allow file locking to work locally.
    /// Hence these are only interesting for network filesystems and similar.
    fn setlk(&mut self, _req: &Request, _ino: u64, _fh: u64, _lock_owner: u64, _start: u64, _end: u64, _typ: u32, _pid: u32, _sleep: bool, reply: ReplyEmpty) {
        reply.error(ENOSYS);
    }

    /// Map block index within file to block index within device.
    /// Note: This makes sense only for block device backed filesystems mounted
    /// with the 'blkdev' option
    fn bmap(&mut self, _req: &Request, _ino: u64, _blocksize: u32, _idx: u64, reply: ReplyBmap) {
        reply.error(ENOSYS);
    }

    /// macOS only: Rename the volume. Set fuse_init_out.flags during init to
    /// FUSE_VOL_RENAME to enable
    #[cfg(target_os = "macos")]
    fn setvolname(&mut self, _req: &Request, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(ENOSYS);
    }

    /// macOS only (undocumented)
    #[cfg(target_os = "macos")]
    fn exchange(&mut self, _req: &Request, _parent: u64, _name: &OsStr, _newparent: u64, _newname: &OsStr, _options: u64, reply: ReplyEmpty) {
        reply.error(ENOSYS);
    }

    /// macOS only: Query extended times (bkuptime and crtime). Set fuse_init_out.flags
    /// during init to FUSE_XTIMES to enable
    #[cfg(target_os = "macos")]
    fn getxtimes(&mut self, _req: &Request, _ino: u64, reply: ReplyXTimes) {
        reply.error(ENOSYS);
    }
}
