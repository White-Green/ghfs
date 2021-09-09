use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::RwLock as std_RwLock;

use fuse::{FileAttr, Filesystem, FileType, ReplyAttr, ReplyBmap, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyLock, ReplyOpen, ReplyStatfs, ReplyWrite, ReplyXattr, Request};
use libc::ENOSYS;
use log::info;
use time::Timespec;
use tokio::sync::{mpsc, OwnedRwLockWriteGuard, RwLock};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::github_filesystem::github_api::{GetBlobContentError, GitHubApiRoot, TreeItem};
use crate::github_filesystem::http::HTTPWrapper;

pub(crate) mod github_api;
pub(crate) mod http;

pub(crate) fn mount(url: &str, mount_point: &str, username: &str, password: &str) -> anyhow::Result<()> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let http = HTTPWrapper::new(reqwest::Client::new(), username, password);
    let api = GitHubApiRoot::from_repository_url(url)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let guard = runtime.enter();
    runtime.spawn(async move { operation_handler(receiver, http, api).await.expect("") });
    fuse::mount(GitHubFS { sender }, &mount_point, &[]).expect("failed to mount()");
    drop(guard);
    Ok(())
}

async fn operation_handler(mut receiver: UnboundedReceiver<Message>, http: HTTPWrapper, api_root: GitHubApiRoot) -> anyhow::Result<()> {
    let http = Arc::new(http);
    let mut files = HashMap::new();
    let root_url = api_root.get_repository_tree_url(&http).await?;
    files.insert(1, create_github_directory(1, 0, 0, 0, OsString::new(), tokio::spawn(root_url.into_tree_items(Arc::clone(&http)))));
    let mut children = HashMap::new();
    children.insert(1, HashSet::new());

    let resources = Arc::new(RwLock::new((files, children)));
    let inode_max = Arc::new(AtomicU64::new(2));
    while let Some(message) = receiver.recv().await {
        let resources = Arc::clone(&resources);
        let inode_max = Arc::clone(&inode_max);
        let http = Arc::clone(&http);
        tokio::spawn(async move {
            message_handler_inner(message, resources, inode_max, http).await.expect("");
        });
    }
    Ok(())
}

async fn message_handler_inner(message: Message, resources: Arc<RwLock<(HashMap<u64, FileData>, HashMap<u64, HashSet<u64>>)>>, inode_max: Arc<AtomicU64>, http: Arc<HTTPWrapper>) -> anyhow::Result<()> {
    let Message { unique: _, uid, gid, pid: _, operation } = message;
    match operation {
        Operation::Lookup { parent, name, reply } => {
            if parent == 1 && name == ".." {
                reply.entry(&Timespec::new(1, 0), &resources.read().await.0[&1].attr(), 0);
            } else {
                let resources = resources.write_owned().await;
                let mut resources = load_directory(resources, inode_max, parent, uid, gid, http).await?;
                let (files, children) = &mut *resources;
                let option = children.get(&parent).into_iter().flatten().filter_map(|inode| files.get(inode)).find(|file| file.name == name);
                if let Some(file) = option {
                    file.lookup_count.fetch_add(1, atomic::Ordering::Relaxed);
                    reply.entry(&Timespec::new(0, 0), &file.attr(), 0);
                } else {
                    reply.error(libc::ENOENT);
                }
            }
        }
        Operation::Forget { ino: _, nlookup: _ } => {}
        Operation::Getattr { ino, reply } => {
            let (files, _children) = &*resources.read().await;
            if let Some(file) = files.get(&ino) {
                reply.attr(&Timespec::new(1, 0), &file.attr());
            } else {
                reply.error(ENOSYS);
            }
        }
        Operation::Setattr {
            ino,
            mode,
            uid,
            gid,
            size,
            atime,
            mtime,
            fh: _,
            crtime,
            chgtime: _,
            bkuptime: _,
            flags,
            reply,
        } => {
            let (files, _children) = &mut *resources.write().await;
            //TODO:permission
            if let Some(file) = files.get_mut(&ino) {
                let time = time::now().to_timespec();
                if let Some(n) = mode {
                    file.attr.perm = n as u16;
                }
                if let Some(n) = uid {
                    file.attr.uid = n;
                }
                if let Some(n) = gid {
                    file.attr.gid = n;
                }
                if let Some(n) = size {
                    if n != file.attr.size {
                        match &mut file.data {
                            GitHubFileData::Raw(vec) => {
                                file.attr.size = n;
                                vec.resize(n as usize, 0);
                                file.attr.update_mtime(time);
                            }
                            _ => {}
                        }
                    }
                };
                if let Some(n) = atime {
                    file.attr.update_atime(n);
                }
                if let Some(n) = mtime {
                    file.attr.update_mtime(n);
                }
                if let Some(n) = crtime {
                    file.attr.crtime = n;
                }
                if let Some(n) = flags {
                    file.attr.flags = n;
                }
                file.attr.update_ctime(time);
                reply.attr(&Timespec::new(1, 0), &file.attr());
            } else {
                reply.error(ENOSYS);
            }
        }
        Operation::Readlink { ino: _, reply } => {
            reply.error(ENOSYS);
        }
        Operation::Mknod { parent: _, name: _, mode: _, rdev: _, reply } => {
            reply.error(ENOSYS);
        }
        Operation::Mkdir { parent, name, mode: _, reply } => {
            let (files, children) = &mut *resources.write().await;
            let ino = inode_max.fetch_add(1, atomic::Ordering::AcqRel);
            let file = create_directory(ino, parent, uid, gid, name.to_os_string());
            reply.entry(&Timespec::new(1, 0), &file.attr(), 0);
            assert!(files.insert(ino, file).is_none());
            if let Some(parent) = files.get_mut(&parent) {
                let time = time::now().to_timespec();
                parent.attr.update_mtime(time);
                parent.attr.update_ctime(time);
            }
            children.entry(parent).or_default().insert(ino);
        }
        Operation::Unlink { parent, name, reply } => {
            //TODO:permission
            let (files, children) = &mut *resources.write().await;
            let option = children.get(&parent).into_iter().flatten().filter_map(|ino| files.get(ino)).find(|file| file.name == name);
            if let Some(FileData {
                            data: GitHubFileData::Raw(_) | GitHubFileData::None,
                            attr: EditableFileAttr { ino, kind: FileType::RegularFile, .. },
                            ..
                        }) = option
            {
                let ino = *ino;
                files.remove(&ino);
                if let Some(parent) = files.get_mut(&parent) {
                    let time = time::now().to_timespec();
                    parent.attr.update_mtime(time);
                    parent.attr.update_ctime(time);
                }
                reply.ok();
            } else {
                reply.error(ENOSYS);
            }
        }
        Operation::Rmdir { parent, name, reply } => {
            //TODO:permission
            let (files, children) = &mut *resources.write().await;
            let option = children.get(&parent).into_iter().flatten().filter_map(|ino| files.get(ino)).find(|file| file.name == name);
            if let Some(FileData {
                            data: GitHubFileData::None,
                            attr: EditableFileAttr { ino, kind: FileType::Directory, .. },
                            ..
                        }) = option
            {
                let ino = *ino;
                files.remove(&ino);
                if let Some(parent) = files.get_mut(&parent) {
                    let time = time::now().to_timespec();
                    parent.attr.update_mtime(time);
                    parent.attr.update_ctime(time);
                }
                reply.ok();
            } else {
                reply.error(ENOSYS);
            }
        }
        Operation::Symlink { parent: _, name: _, link: _, reply } => {
            reply.error(ENOSYS);
        }
        Operation::Rename { parent, name, newparent, newname, reply } => {
            //TODO:permission
            let (files, children) = &mut *resources.write().await;
            let option = children.get(&parent).into_iter().flatten().filter_map(|ino| files.get(ino)).find(|file| file.name == name);
            if let Some(FileData {
                            data: GitHubFileData::Raw(_) | GitHubFileData::None,
                            attr: EditableFileAttr { ino, .. },
                            ..
                        }) = option
            {
                let ino = *ino;
                let time = time::now().to_timespec();
                if let Some(parent) = files.get_mut(&parent) {
                    parent.attr.update_mtime(time);
                    parent.attr.update_ctime(time);
                }
                if let Some(parent) = files.get_mut(&newparent) {
                    parent.attr.update_mtime(time);
                    parent.attr.update_ctime(time);
                }
                let file = files.get_mut(&ino).unwrap();
                file.attr.update_ctime(time);
                file.parent = newparent;
                file.name = newname;
                if let Some(child) = children.get_mut(&parent) {
                    child.remove(&ino);
                }
                children.entry(newparent).or_default().insert(ino);
                reply.ok();
            } else {
                reply.error(ENOSYS);
            }
        }
        Operation::Link { ino: _, newparent: _, newname: _, reply } => {
            reply.error(ENOSYS);
        }
        Operation::Open { ino: _, flags, reply } => {
            reply.opened(0, flags);
        }
        Operation::Read { ino, fh: _, offset, size, reply } => {
            //TODO:permission
            let offset = offset as usize;
            let size = size as usize;
            let (files, _children) = &mut *resources.write().await;
            if let Some(file) = files.get_mut(&ino) {
                if let data @ GitHubFileData::FetchingGitHubFile(_) = &mut file.data {
                    if let GitHubFileData::FetchingGitHubFile(handle) = std::mem::replace(data, GitHubFileData::None) {
                        *data = GitHubFileData::Raw(handle.await??);
                    } else {
                        unreachable!()
                    }
                }

                if let GitHubFileData::Raw(data) = &file.data {
                    if offset < data.len() {
                        reply.data(&data[offset..(offset + size).min(data.len())]);
                    } else {
                        reply.data(&[]);
                    }
                } else {
                    reply.error(ENOSYS);
                }
            } else {
                reply.error(ENOSYS);
            }
        }
        Operation::Write { ino, fh: _, offset, data, flags: _, reply } => {
            //TODO:permission
            let (files, _children) = &mut *resources.write().await;
            if let Some(file) = files.get_mut(&ino) {
                if let data @ GitHubFileData::FetchingGitHubFile(_) = &mut file.data {
                    if let GitHubFileData::FetchingGitHubFile(handle) = std::mem::replace(data, GitHubFileData::None) {
                        *data = GitHubFileData::Raw(handle.await??);
                    } else {
                        unreachable!()
                    }
                }

                if let FileData {
                    parent: _,
                    attr,
                    name: _,
                    data: GitHubFileData::Raw(file),
                    lookup_count: _,
                    removed: _,
                } = file
                {
                    let time = time::now().to_timespec();
                    attr.update_mtime(time);
                    attr.update_ctime(time);
                    if file.len() < offset as usize + data.len() {
                        file.resize(offset as usize + data.len(), 0);
                        attr.size = file.len() as u64;
                    }
                    file[offset as usize..offset as usize + data.len()].clone_from_slice(&data);
                    reply.written(data.len() as u32);
                } else {
                    reply.error(ENOSYS);
                }
            } else {
                reply.error(ENOSYS);
            }
        }
        Operation::Flush { ino: _, fh: _, lock_owner: _, reply } => {
            reply.ok();
        }
        Operation::Release { ino: _, fh: _, flags: _, lock_owner: _, flush: _, reply } => {
            // //実際の削除はここでやる
            // if let Some(&FileData { removed: true, .. }) = self.files.get(&ino) {
            //     self.files.remove(&ino);
            // }
            reply.ok();
        }
        Operation::Fsync { ino: _, fh: _, datasync: _, reply } => {
            reply.error(ENOSYS);
        }
        Operation::Opendir { ino: _, flags, reply } => {
            reply.opened(0, flags);
        }
        Operation::Readdir { ino, fh: _, offset, mut reply } => {
            let resources = resources.write_owned().await;
            let mut resources = load_directory(resources, inode_max, ino, uid, gid, http).await?;
            let (files, children) = &mut *resources;
            let offset = offset as usize;
            let current = OsString::from(".");
            let parent = OsString::from("..");
            if let Some(file) = files.get(&ino) {
                let time = time::now().to_timespec();
                file.attr.update_atime(time);
            }
            let iter = files
                .get(&ino)
                .map_or(Vec::new(), |file| vec![Some((&file.attr, &current)), if ino == 1 { Some((&file.attr, &parent)) } else { files.get(&file.parent).map_or(None, |file| Some((&file.attr, &parent))) }])
                .into_iter()
                .flatten()
                .chain(children.get(&ino).into_iter().flatten().filter_map(|ino| files.get(ino).map(|file| (&file.attr, &file.name))));
            for (i, (f, n)) in iter.enumerate().skip(offset) {
                if reply.add(f.ino, i as i64 + 1, f.kind, n) {
                    break;
                }
            }
            reply.ok();
        }
        Operation::Releasedir { ino: _, fh: _, flags: _, reply } => {
            // if let Some(&FileData { removed: true, .. }) = self.files.get(&ino) {
            //     self.files.remove(&ino);
            // }
            reply.ok();
        }
        Operation::Fsyncdir { ino: _, fh: _, datasync: _, reply } => {
            reply.error(ENOSYS);
        }
        Operation::Statfs { ino: _, reply } => {
            reply.statfs(0, 0, 0, 0, 0, 512, 255, 0);
        }
        Operation::Setxattr { ino: _, name: _, value: _, flags: _, position: _, reply } => {
            reply.error(ENOSYS);
        }
        Operation::Getxattr { ino: _, name: _, size: _, reply } => {
            reply.error(ENOSYS);
        }
        Operation::Listxattr { ino: _, size: _, reply } => {
            reply.error(ENOSYS);
        }
        Operation::Removexattr { ino: _, name: _, reply } => {
            reply.error(ENOSYS);
        }
        Operation::Access { ino: _, mask: _, reply } => {
            reply.ok();
            // reply.error(ENOSYS);
        }
        Operation::Create { parent, name, mode: _, flags, reply } => {
            //TODO:permission
            let (files, children) = &mut *resources.write().await;
            let ino = inode_max.fetch_add(1, atomic::Ordering::AcqRel);
            let file = create_empty_file(ino, parent, uid, gid, name.to_os_string());
            reply.created(&Timespec::new(1, 0), &file.attr(), 0, 0, flags);
            assert!(files.insert(ino, file).is_none());
            if let Some(parent) = files.get_mut(&parent) {
                let time = time::now().to_timespec();
                parent.attr.update_mtime(time);
                parent.attr.update_ctime(time);
            }
            children.entry(parent).or_default().insert(ino);
        }
        Operation::Getlk { ino: _, fh: _, lock_owner: _, start: _, end: _, typ: _, pid: _, reply } => {
            reply.error(ENOSYS);
        }
        Operation::Setlk {
            ino: _,
            fh: _,
            lock_owner: _,
            start: _,
            end: _,
            typ: _,
            pid: _,
            sleep: _,
            reply,
        } => {
            reply.error(ENOSYS);
        }
        Operation::Bmap { ino: _, blocksize: _, idx: _, reply } => {
            reply.error(ENOSYS);
        }
    }
    Ok(())
}

async fn load_directory(mut resources: OwnedRwLockWriteGuard<(HashMap<u64, FileData>, HashMap<u64, HashSet<u64>>)>, inode_max: Arc<AtomicU64>, directory: u64, uid: u32, gid: u32, http: Arc<HTTPWrapper>) -> anyhow::Result<OwnedRwLockWriteGuard<(HashMap<u64, FileData>, HashMap<u64, HashSet<u64>>)>> {
    let (files, children) = &mut *resources;
    let file = files.get_mut(&directory).ok_or(anyhow::Error::msg(""))?;
    if let GitHubFileData::FetchingGitHubDirectory(_) = &mut file.data {
        if let GitHubFileData::FetchingGitHubDirectory(handle) = std::mem::replace(&mut file.data, GitHubFileData::None) {
            let directory_children = handle.await??;
            for item in directory_children {
                let ino = inode_max.fetch_add(1, atomic::Ordering::AcqRel);
                match item {
                    TreeItem::Blob(blob) => {
                        assert!(files.insert(ino, create_github_file(ino, blob.size() as u64, directory, uid, gid, OsString::from(blob.path()), tokio::spawn(blob.into_content(Arc::clone(&http))))).is_none());
                    }
                    TreeItem::Tree(tree) => {
                        assert!(files.insert(ino, create_github_directory(ino, directory, uid, gid, OsString::from(tree.path()), tokio::spawn(tree.into_tree_items(Arc::clone(&http))))).is_none());
                    }
                }
                children.entry(directory).or_default().insert(ino);
            }
        } else {
            unreachable!()
        }
    }
    Ok(resources)
}

fn create_empty_file(ino: u64, parent: u64, uid: u32, gid: u32, name: OsString) -> FileData {
    let t = time::now().to_timespec();
    FileData {
        parent,
        attr: EditableFileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: std_RwLock::new(t),
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
        lookup_count: AtomicUsize::new(0),
        removed: false,
    }
}

fn create_directory(ino: u64, parent: u64, uid: u32, gid: u32, name: OsString) -> FileData {
    let t = time::now().to_timespec();
    FileData {
        parent,
        attr: EditableFileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: std_RwLock::new(t),
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
        lookup_count: AtomicUsize::new(0),
        removed: false,
    }
}

fn create_github_file(ino: u64, size: u64, parent: u64, uid: u32, gid: u32, name: OsString, content: JoinHandle<Result<Vec<u8>, GetBlobContentError>>) -> FileData {
    let t = time::now().to_timespec();
    FileData {
        parent,
        attr: EditableFileAttr {
            ino,
            size,
            blocks: 0,
            atime: std_RwLock::new(t),
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
        data: GitHubFileData::FetchingGitHubFile(content),
        lookup_count: AtomicUsize::new(0),
        removed: false,
    }
}

fn create_github_directory(ino: u64, parent: u64, uid: u32, gid: u32, name: OsString, directory_items: JoinHandle<Result<Vec<TreeItem>, reqwest::Error>>) -> FileData {
    let t = time::now().to_timespec();
    FileData {
        parent,
        attr: EditableFileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: std_RwLock::new(t),
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
        data: GitHubFileData::FetchingGitHubDirectory(directory_items),
        lookup_count: AtomicUsize::new(0),
        removed: false,
    }
}

#[derive(Debug)]
enum GitHubFileData {
    Raw(Vec<u8>),
    FetchingGitHubFile(JoinHandle<Result<Vec<u8>, GetBlobContentError>>),
    FetchingGitHubDirectory(JoinHandle<Result<Vec<TreeItem>, reqwest::Error>>),
    None,
}

#[derive(Debug)]
struct EditableFileAttr {
    /// Inode number
    pub ino: u64,
    /// Size in bytes
    pub size: u64,
    /// Size in blocks
    pub blocks: u64,
    /// Time of last access
    pub atime: std_RwLock<Timespec>,
    /// Time of last modification
    pub mtime: Timespec,
    /// Time of last change
    pub ctime: Timespec,
    /// Time of creation (macOS only)
    pub crtime: Timespec,
    /// Kind of file (directory, file, pipe, etc)
    pub kind: FileType,
    /// Permissions
    pub perm: u16,
    /// Number of hard links
    pub nlink: u32,
    /// User id
    pub uid: u32,
    /// Group id
    pub gid: u32,
    /// Rdev
    pub rdev: u32,
    /// Flags (macOS only, see chflags(2))
    pub flags: u32,
}

impl EditableFileAttr {
    fn update_atime(&self, time: Timespec) {
        *self.atime.write().unwrap() = time;
    }

    fn update_mtime(&mut self, time: Timespec) {
        self.mtime = time;
    }

    fn update_ctime(&mut self, time: Timespec) {
        self.ctime = time;
    }
}

#[derive(Debug)]
struct FileData {
    parent: u64,
    attr: EditableFileAttr,
    name: OsString,
    data: GitHubFileData,
    lookup_count: AtomicUsize,
    removed: bool,
}

impl FileData {
    fn attr(&self) -> FileAttr {
        let EditableFileAttr {
            ino,
            size,
            blocks,
            ref atime,
            mtime,
            ctime,
            crtime,
            kind,
            perm,
            nlink,
            uid,
            gid,
            rdev,
            flags,
        } = self.attr;
        FileAttr {
            ino,
            size,
            blocks,
            atime: *atime.read().unwrap(),
            mtime,
            ctime,
            crtime,
            kind,
            perm,
            nlink,
            uid,
            gid,
            rdev,
            flags,
        }
    }
}

struct GitHubFS {
    sender: UnboundedSender<Message>,
}

const APP_NAME: &str = "ghfs";

// impl GitHubFS {
//     fn load_directory_if_not_loaded(&mut self, uid: u32, gid: u32, parent: u64) {
//         if let Some(FileData { data: GitHubFileData::GitHubDirectory { url, fetched: false }, .. }) = self.files.get(&parent) {
//             let url = url.clone();
//             let files = self.load_child_files_from_github(&url, parent, uid, gid);
//             if let Some(FileData { data: GitHubFileData::GitHubDirectory { fetched, .. }, .. }) = self.files.get_mut(&parent) {
//                 *fetched = true;
//             } else { unreachable!(); }
//             for file in files {
//                 self.insert_file(file, parent);
//             }
//         }
//     }
//     fn get_file_data(&mut self, ino: u64) -> Option<&Vec<u8>> {
//         let file = match self.files.get(&ino) {
//             None => return None,
//             Some(file) => file,
//         };
//         match &file.data {
//             GitHubFileData::GitHubFile { url } => {
//                 if self.cache.contains(ino) {
//                     self.cache.get_cache(ino)
//                 } else {
//                     Some(self.cache.insert_cache(ino, self.load_file_from_github(url)))
//                 }
//             }
//             GitHubFileData::Raw(vec) => Some(vec),
//             _ => { None }
//         }
//     }
//     fn insert_file(&mut self, file: FileData, parent: u64) {
//         if let Some(children) = self.children.get_mut(&parent) {
//             children.insert(file.attr.ino);
//         } else {
//             let mut children = HashSet::with_capacity(1);
//             children.insert(file.attr.ino);
//             self.children.insert(parent, children);
//         }
//         self.files.insert(file.attr.ino, file);
//     }
//     fn get_json(&self, url: &str) -> Box<JFObject> {
//         json_request(&self.client, url, &self.username, &self.password)
//     }
//     fn load_file_from_github(&self, url: &str) -> Vec<u8> {
//         let json = self.get_json(url);
//         base64::decode(&json["content"].unwrap_string().replace("\\n", "")).unwrap()
//     }
//     fn load_child_files_from_github(&mut self, url: &str, parent: u64, uid: u32, gid: u32) -> Vec<FileData> {
//         let json = self.get_json(url);
//         json["tree"].unwrap_vec().iter().map(|file| {
//             match file["type"].unwrap_string().as_str() {
//                 "blob" => {
//                     create_github_file(
//                         self.next_ino(),
//                         *file["size"].unwrap_i64() as u64,
//                         parent,
//                         uid,
//                         gid,
//                         OsString::from(file["path"].unwrap_string()),
//                         file["url"].unwrap_string().clone())
//                 }
//                 "tree" => {
//                     create_github_directory(
//                         self.next_ino(),
//                         parent,
//                         uid,
//                         gid,
//                         OsString::from(file["path"].unwrap_string()),
//                         file["url"].unwrap_string().clone())
//                 }
//                 _ => unimplemented!()
//             }
//         }).collect()
//     }
//     fn next_ino(&mut self) -> u64 {
//         self.inode_max += 1;
//         self.inode_max
//     }
// }

#[derive(Debug)]
struct Message {
    unique: u64,
    uid: u32,
    gid: u32,
    pid: u32,
    operation: Operation,
}

impl Message {
    fn new(req: &Request, operation: Operation) -> Self {
        Message {
            unique: req.unique(),
            uid: req.uid(),
            gid: req.gid(),
            pid: req.pid(),
            operation,
        }
    }
}

#[derive(Debug)]
enum Operation {
    Lookup {
        parent: u64,
        name: OsString,
        reply: ReplyEntry,
    },
    Forget {
        ino: u64,
        nlookup: u64,
    },
    Getattr {
        ino: u64,
        reply: ReplyAttr,
    },
    Setattr {
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<Timespec>,
        mtime: Option<Timespec>,
        fh: Option<u64>,
        crtime: Option<Timespec>,
        chgtime: Option<Timespec>,
        bkuptime: Option<Timespec>,
        flags: Option<u32>,
        reply: ReplyAttr,
    },
    Readlink {
        ino: u64,
        reply: ReplyData,
    },
    Mknod {
        parent: u64,
        name: OsString,
        mode: u32,
        rdev: u32,
        reply: ReplyEntry,
    },
    Mkdir {
        parent: u64,
        name: OsString,
        mode: u32,
        reply: ReplyEntry,
    },
    Unlink {
        parent: u64,
        name: OsString,
        reply: ReplyEmpty,
    },
    Rmdir {
        parent: u64,
        name: OsString,
        reply: ReplyEmpty,
    },
    Symlink {
        parent: u64,
        name: OsString,
        link: PathBuf,
        reply: ReplyEntry,
    },
    Rename {
        parent: u64,
        name: OsString,
        newparent: u64,
        newname: OsString,
        reply: ReplyEmpty,
    },
    Link {
        ino: u64,
        newparent: u64,
        newname: OsString,
        reply: ReplyEntry,
    },
    Open {
        ino: u64,
        flags: u32,
        reply: ReplyOpen,
    },
    Read {
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        reply: ReplyData,
    },
    Write {
        ino: u64,
        fh: u64,
        offset: i64,
        data: Vec<u8>,
        flags: u32,
        reply: ReplyWrite,
    },
    Flush {
        ino: u64,
        fh: u64,
        lock_owner: u64,
        reply: ReplyEmpty,
    },
    Release {
        ino: u64,
        fh: u64,
        flags: u32,
        lock_owner: u64,
        flush: bool,
        reply: ReplyEmpty,
    },
    Fsync {
        ino: u64,
        fh: u64,
        datasync: bool,
        reply: ReplyEmpty,
    },
    Opendir {
        ino: u64,
        flags: u32,
        reply: ReplyOpen,
    },
    Readdir {
        ino: u64,
        fh: u64,
        offset: i64,
        reply: ReplyDirectory,
    },
    Releasedir {
        ino: u64,
        fh: u64,
        flags: u32,
        reply: ReplyEmpty,
    },
    Fsyncdir {
        ino: u64,
        fh: u64,
        datasync: bool,
        reply: ReplyEmpty,
    },
    Statfs {
        ino: u64,
        reply: ReplyStatfs,
    },
    Setxattr {
        ino: u64,
        name: OsString,
        value: Vec<u8>,
        flags: u32,
        position: u32,
        reply: ReplyEmpty,
    },
    Getxattr {
        ino: u64,
        name: OsString,
        size: u32,
        reply: ReplyXattr,
    },
    Listxattr {
        ino: u64,
        size: u32,
        reply: ReplyXattr,
    },
    Removexattr {
        ino: u64,
        name: OsString,
        reply: ReplyEmpty,
    },
    Access {
        ino: u64,
        mask: u32,
        reply: ReplyEmpty,
    },
    Create {
        parent: u64,
        name: OsString,
        mode: u32,
        flags: u32,
        reply: ReplyCreate,
    },
    Getlk {
        ino: u64,
        fh: u64,
        lock_owner: u64,
        start: u64,
        end: u64,
        typ: u32,
        pid: u32,
        reply: ReplyLock,
    },
    Setlk {
        ino: u64,
        fh: u64,
        lock_owner: u64,
        start: u64,
        end: u64,
        typ: u32,
        pid: u32,
        sleep: bool,
        reply: ReplyEmpty,
    },
    Bmap {
        ino: u64,
        blocksize: u32,
        idx: u64,
        reply: ReplyBmap,
    },
}

const SEND_ERROR_MESSAGE: &'static str = "";

impl Filesystem for GitHubFS {
    fn init(&mut self, _req: &Request) -> Result<(), c_int> {
        info!("いにしゃらいず");
        Ok(())
    }
    fn destroy(&mut self, _req: &Request) {
        info!("ですとろーい");
    }
    fn lookup(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        self.sender.send(Message::new(req, Operation::Lookup { parent, name: name.to_os_string(), reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn forget(&mut self, req: &Request, ino: u64, nlookup: u64) {
        self.sender.send(Message::new(req, Operation::Forget { ino, nlookup })).expect(SEND_ERROR_MESSAGE);
    }
    fn getattr(&mut self, req: &Request, ino: u64, reply: ReplyAttr) {
        self.sender.send(Message::new(req, Operation::Getattr { ino, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn setattr(&mut self, req: &Request, ino: u64, mode: Option<u32>, uid: Option<u32>, gid: Option<u32>, size: Option<u64>, atime: Option<Timespec>, mtime: Option<Timespec>, fh: Option<u64>, crtime: Option<Timespec>, chgtime: Option<Timespec>, bkuptime: Option<Timespec>, flags: Option<u32>, reply: ReplyAttr) {
        self.sender
            .send(Message::new(
                req,
                Operation::Setattr {
                    ino,
                    mode,
                    uid,
                    gid,
                    size,
                    atime,
                    mtime,
                    fh,
                    crtime,
                    chgtime,
                    bkuptime,
                    flags,
                    reply,
                },
            ))
            .expect(SEND_ERROR_MESSAGE);
    }
    fn readlink(&mut self, req: &Request, ino: u64, reply: ReplyData) {
        self.sender.send(Message::new(req, Operation::Readlink { ino, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn mknod(&mut self, req: &Request, parent: u64, name: &OsStr, mode: u32, rdev: u32, reply: ReplyEntry) {
        self.sender.send(Message::new(req, Operation::Mknod { parent, name: name.to_os_string(), mode, rdev, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn mkdir(&mut self, req: &Request, parent: u64, name: &OsStr, mode: u32, reply: ReplyEntry) {
        self.sender.send(Message::new(req, Operation::Mkdir { parent, name: name.to_os_string(), mode, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn unlink(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        self.sender.send(Message::new(req, Operation::Unlink { parent, name: name.to_os_string(), reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn rmdir(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        self.sender.send(Message::new(req, Operation::Rmdir { parent, name: name.to_os_string(), reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn symlink(&mut self, req: &Request, parent: u64, name: &OsStr, link: &Path, reply: ReplyEntry) {
        self.sender.send(Message::new(req, Operation::Symlink { parent, name: name.to_os_string(), link: link.to_path_buf(), reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn rename(&mut self, req: &Request, parent: u64, name: &OsStr, newparent: u64, newname: &OsStr, reply: ReplyEmpty) {
        self.sender
            .send(Message::new(
                req,
                Operation::Rename {
                    parent,
                    name: name.to_os_string(),
                    newparent,
                    newname: newname.to_os_string(),
                    reply,
                },
            ))
            .expect(SEND_ERROR_MESSAGE);
    }
    fn link(&mut self, req: &Request, ino: u64, newparent: u64, newname: &OsStr, reply: ReplyEntry) {
        self.sender.send(Message::new(req, Operation::Link { ino, newparent, newname: newname.to_os_string(), reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn open(&mut self, req: &Request, ino: u64, flags: u32, reply: ReplyOpen) {
        self.sender.send(Message::new(req, Operation::Open { ino, flags, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn read(&mut self, req: &Request, ino: u64, fh: u64, offset: i64, size: u32, reply: ReplyData) {
        self.sender.send(Message::new(req, Operation::Read { ino, fh, offset, size, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn write(&mut self, req: &Request, ino: u64, fh: u64, offset: i64, data: &[u8], flags: u32, reply: ReplyWrite) {
        self.sender.send(Message::new(req, Operation::Write { ino, fh, offset, data: data.to_vec(), flags, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn flush(&mut self, req: &Request, ino: u64, fh: u64, lock_owner: u64, reply: ReplyEmpty) {
        self.sender.send(Message::new(req, Operation::Flush { ino, fh, lock_owner, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn release(&mut self, req: &Request, ino: u64, fh: u64, flags: u32, lock_owner: u64, flush: bool, reply: ReplyEmpty) {
        self.sender.send(Message::new(req, Operation::Release { ino, fh, flags, lock_owner, flush, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn fsync(&mut self, req: &Request, ino: u64, fh: u64, datasync: bool, reply: ReplyEmpty) {
        self.sender.send(Message::new(req, Operation::Fsync { ino, fh, datasync, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn opendir(&mut self, req: &Request, ino: u64, flags: u32, reply: ReplyOpen) {
        self.sender.send(Message::new(req, Operation::Opendir { ino, flags, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn readdir(&mut self, req: &Request, ino: u64, fh: u64, offset: i64, reply: ReplyDirectory) {
        self.sender.send(Message::new(req, Operation::Readdir { ino, fh, offset, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn releasedir(&mut self, req: &Request, ino: u64, fh: u64, flags: u32, reply: ReplyEmpty) {
        self.sender.send(Message::new(req, Operation::Releasedir { ino, fh, flags, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn fsyncdir(&mut self, req: &Request, ino: u64, fh: u64, datasync: bool, reply: ReplyEmpty) {
        self.sender.send(Message::new(req, Operation::Fsyncdir { ino, fh, datasync, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn statfs(&mut self, req: &Request, ino: u64, reply: ReplyStatfs) {
        self.sender.send(Message::new(req, Operation::Statfs { ino, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn setxattr(&mut self, req: &Request, ino: u64, name: &OsStr, value: &[u8], flags: u32, position: u32, reply: ReplyEmpty) {
        self.sender
            .send(Message::new(
                req,
                Operation::Setxattr {
                    ino,
                    name: name.to_os_string(),
                    value: value.to_vec(),
                    flags,
                    position,
                    reply,
                },
            ))
            .expect(SEND_ERROR_MESSAGE);
    }
    fn getxattr(&mut self, req: &Request, ino: u64, name: &OsStr, size: u32, reply: ReplyXattr) {
        self.sender.send(Message::new(req, Operation::Getxattr { ino, name: name.to_os_string(), size, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn listxattr(&mut self, req: &Request, ino: u64, size: u32, reply: ReplyXattr) {
        self.sender.send(Message::new(req, Operation::Listxattr { ino, size, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn removexattr(&mut self, req: &Request, ino: u64, name: &OsStr, reply: ReplyEmpty) {
        self.sender.send(Message::new(req, Operation::Removexattr { ino, name: name.to_os_string(), reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn access(&mut self, req: &Request, ino: u64, mask: u32, reply: ReplyEmpty) {
        self.sender.send(Message::new(req, Operation::Access { ino, mask, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn create(&mut self, req: &Request, parent: u64, name: &OsStr, mode: u32, flags: u32, reply: ReplyCreate) {
        self.sender.send(Message::new(req, Operation::Create { parent, name: name.to_os_string(), mode, flags, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn getlk(&mut self, req: &Request, ino: u64, fh: u64, lock_owner: u64, start: u64, end: u64, typ: u32, pid: u32, reply: ReplyLock) {
        self.sender.send(Message::new(req, Operation::Getlk { ino, fh, lock_owner, start, end, typ, pid, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn setlk(&mut self, req: &Request, ino: u64, fh: u64, lock_owner: u64, start: u64, end: u64, typ: u32, pid: u32, sleep: bool, reply: ReplyEmpty) {
        self.sender.send(Message::new(req, Operation::Setlk { ino, fh, lock_owner, start, end, typ, pid, sleep, reply })).expect(SEND_ERROR_MESSAGE);
    }
    fn bmap(&mut self, req: &Request, ino: u64, blocksize: u32, idx: u64, reply: ReplyBmap) {
        self.sender.send(Message::new(req, Operation::Bmap { ino, blocksize, idx, reply })).expect(SEND_ERROR_MESSAGE);
    }
    #[cfg(target_os = "macos")]
    fn setvolname(&mut self, _req: &Request, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(ENOSYS);
    }
    #[cfg(target_os = "macos")]
    fn exchange(&mut self, _req: &Request, _parent: u64, _name: &OsStr, _newparent: u64, _newname: &OsStr, _options: u64, reply: ReplyEmpty) {
        reply.error(ENOSYS);
    }
    #[cfg(target_os = "macos")]
    fn getxtimes(&mut self, _req: &Request, _ino: u64, reply: ReplyXTimes) {
        reply.error(ENOSYS);
    }
}
