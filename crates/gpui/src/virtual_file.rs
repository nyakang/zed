//! Deferred file content for native drag destinations. Payload resolution performs no IO.

use smallvec::SmallVec;
use std::{ffi::OsString, fmt, io, sync::Arc, time::SystemTime};

/// Maximum content request; adapters split larger reads into bounded chunks.
pub const VIRTUAL_FILE_CHUNK_SIZE: usize = 64 * 1024;

/// A reusable deferred source, independent of the application's transport.
pub trait VirtualFileProvider: Send + Sync + 'static {
    /// Create an independent stream without blocking IO. Platform workers request content.
    fn open(&self) -> io::Result<Box<dyn VirtualFileStream>>;
    /// Cancel every open stream and prevent subsequent opens. Must not block.
    fn cancel(&self);
}

/// A seekable content handle. Dropping it must cancel outstanding work.
pub trait VirtualFileStream: Send + 'static {
    /// Read at an explicit offset, returning zero only at EOF. The buffer is at
    /// most `VIRTUAL_FILE_CHUNK_SIZE` bytes. Propagate cancellation and IO failures.
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize>;
    /// Wake pending requests. Must not block.
    fn cancel(&self);
}

/// Metadata advertised before a destination requests content.
#[derive(Clone)]
pub struct VirtualFileDescriptor {
    /// A single safe filename, never a destination path.
    pub name: OsString,
    /// Optional advertised length.
    pub size: Option<u64>,
    /// Optional last modification time.
    pub modified_at: Option<SystemTime>,
    /// Deferred content provider; GPUI knows nothing about its transport.
    pub provider: Arc<dyn VirtualFileProvider>,
}

impl fmt::Debug for VirtualFileDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtualFileDescriptor")
            .field("name", &self.name)
            .field("size", &self.size)
            .field("modified_at", &self.modified_at)
            .finish_non_exhaustive()
    }
}
impl PartialEq for VirtualFileDescriptor {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.size == other.size
            && self.modified_at == other.modified_at
            && Arc::ptr_eq(&self.provider, &other.provider)
    }
}
impl Eq for VirtualFileDescriptor {}

/// Regular virtual files in content-index order. Native destinations handle conflicts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualFileDragPayload(SmallVec<[VirtualFileDescriptor; 2]>);
impl VirtualFileDragPayload {
    /// Validate every filename before advertising the payload.
    pub fn new(files: impl IntoIterator<Item = VirtualFileDescriptor>) -> io::Result<Self> {
        let files: SmallVec<[VirtualFileDescriptor; 2]> = files.into_iter().collect();
        if files.is_empty() || files.len() > 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid virtual file count",
            ));
        }
        let mut names = std::collections::HashSet::new();
        for file in &files {
            let name = file.name.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "filename is not Unicode")
            })?;
            let stem = name
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            let reserved_port = stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|suffix| {
                    matches!(
                        suffix,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                });
            if name.is_empty()
                || name == "."
                || name == ".."
                || name.ends_with(['.', ' '])
                || name
                    .chars()
                    .any(|c| c.is_control() || "/\\:<>\"|?*".contains(c))
                || name.encode_utf16().count() >= 260
                || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || reserved_port
                || !names.insert(name.to_lowercase())
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsafe or duplicate virtual filename",
                ));
            }
        }
        Ok(Self(files))
    }
    /// Descriptors in native content-index order.
    pub fn files(&self) -> &[VirtualFileDescriptor] {
        &self.0
    }
    /// Cancel all sources without starting new source work.
    pub fn cancel(&self) {
        for file in &self.0 {
            file.provider.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        VirtualFileDescriptor, VirtualFileDragPayload, VirtualFileProvider, VirtualFileStream,
    };
    use std::{
        io,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    struct Source(AtomicUsize);
    impl VirtualFileProvider for Source {
        fn open(&self) -> io::Result<Box<dyn VirtualFileStream>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(io::ErrorKind::Unsupported.into())
        }
        fn cancel(&self) {}
    }
    fn descriptor(name: &str, source: Arc<Source>) -> VirtualFileDescriptor {
        VirtualFileDescriptor {
            name: name.into(),
            size: Some(0),
            modified_at: None,
            provider: source,
        }
    }
    #[test]
    fn metadata_and_cancel_never_open_content() {
        let source = Arc::new(Source(AtomicUsize::new(0)));
        let payload =
            VirtualFileDragPayload::new([descriptor("你好.txt", source.clone())]).unwrap();
        assert_eq!(payload.files()[0].size, Some(0));
        payload.cancel();
        assert_eq!(source.0.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn rejects_paths_devices_and_duplicate_names() {
        let source = Arc::new(Source(AtomicUsize::new(0)));
        for name in [
            "",
            ".",
            "..",
            "../file",
            "a\\b",
            "x:y",
            "NUL.txt",
            "COM1",
            "COM¹.txt",
            "LPT²",
            "a.",
            "a\0b",
        ] {
            assert!(VirtualFileDragPayload::new([descriptor(name, source.clone())]).is_err());
        }
        assert!(
            VirtualFileDragPayload::new([
                descriptor("A.txt", source.clone()),
                descriptor("a.txt", source)
            ])
            .is_err()
        );
    }
}
