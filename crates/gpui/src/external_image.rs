//! Buffers produced outside the window renderer.

#[cfg(target_os = "linux")]
use crate::{DevicePixels, Point};
#[cfg(target_os = "linux")]
use std::{fmt, os::fd::BorrowedFd, path::PathBuf};

#[expect(missing_docs)]
pub enum AtlasTileUpdate<'a> {
    /// BGRA8 rows copied before this call returns.
    Pixels { bytes: &'a [u8], bytes_per_row: u32 },
    #[cfg(target_os = "windows")]
    SharedTexture {
        texture: SharedTexture<'a>,
        origin: crate::Point<crate::DevicePixels>,
    },
    #[cfg(target_os = "linux")]
    Dmabuf {
        buffer: DmabufId,
        origin: Point<DevicePixels>,
    },
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[expect(missing_docs)]
pub struct DmabufId(pub u64);

/// The producer must finish rendering before queuing a copy and must not reuse
/// its buffer until that update's completion callback runs.
#[cfg(target_os = "linux")]
#[derive(Debug)]
#[expect(missing_docs)]
pub struct Dmabuf<'a> {
    pub fd: BorrowedFd<'a>,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub modifier: u64,
    pub offset: u32,
    pub stride: u32,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(missing_docs)]
pub struct DmabufTarget {
    pub render_node: PathBuf,
    pub modifiers: Vec<u64>,
}

/// Dropping the import removes it from future updates. Queued GPU reads retain
/// the imported memory until completion.
#[cfg(target_os = "linux")]
#[expect(missing_docs)]
pub struct DmabufImport {
    pub id: DmabufId,
    release: Option<Box<dyn FnOnce() + Send>>,
}

#[cfg(target_os = "linux")]
impl DmabufImport {
    #[expect(missing_docs)]
    pub fn new(id: DmabufId, release: impl FnOnce() + Send + 'static) -> Self {
        Self {
            id,
            release: Some(Box::new(release)),
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for DmabufImport {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

#[cfg(target_os = "linux")]
impl fmt::Debug for DmabufImport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DmabufImport").field(&self.id).finish()
    }
}

/// The adapter used for Windows shared-texture imports.
#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(missing_docs)]
pub struct DxgiAdapterId {
    pub low: u32,
    pub high: i32,
}

/// A single-plane BGRA8 texture in D3D12 COMMON state, produced on this atlas's
/// adapter. Both NT handles must remain valid until `update_tile` returns.
/// The producer must signal `value` after writing, and must not write again
/// until the atlas update's completion runs. Keyed-mutex resources are unsupported.
#[cfg(target_os = "windows")]
#[expect(missing_docs)]
pub struct SharedTexture<'a> {
    pub resource: std::os::windows::io::BorrowedHandle<'a>,
    pub fence: std::os::windows::io::BorrowedHandle<'a>,
    pub value: u64,
}
