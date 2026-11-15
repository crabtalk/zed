use anyhow::{Context as _, Result, ensure};
use etagere::{BucketedAtlasAllocator, size2};
use gpui::{
    AtlasBackend, AtlasKey, AtlasState, AtlasTextureId, AtlasTextureKind, AtlasTextureList,
    AtlasTile, Bounds, DevicePixels, PlatformAtlas, Point, Size, external_image::AtlasTileUpdate,
};
use parking_lot::Mutex;
use std::{borrow::Cow, ops, sync::Arc};

use crate::WgpuContext;
#[cfg(target_os = "linux")]
use crate::dmabuf::{self, DmabufDevice as _, ImportedBuffer};
#[cfg(target_os = "windows")]
use crate::dxgi::ImportedTexture;
#[cfg(target_os = "linux")]
use collections::FxHashMap;
#[cfg(target_os = "linux")]
use gpui::external_image::{Dmabuf, DmabufId, DmabufImport, DmabufTarget};

fn device_size_to_etagere(size: Size<DevicePixels>) -> etagere::Size {
    size2(size.width.0, size.height.0)
}

fn etagere_point_to_device(point: etagere::Point) -> Point<DevicePixels> {
    Point {
        x: DevicePixels(point.x),
        y: DevicePixels(point.y),
    }
}

pub struct WgpuAtlas(Arc<Mutex<AtlasState<WgpuAtlasTextures>>>);

struct CompletionPoller {
    #[cfg(not(target_family = "wasm"))]
    sender: std::sync::mpsc::SyncSender<()>,
}

impl CompletionPoller {
    fn new(device: Arc<wgpu::Device>) -> Self {
        #[cfg(not(target_family = "wasm"))]
        {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("gpui-gpu-completion".into())
                .spawn(move || {
                    while receiver.recv().is_ok() {
                        let _ = device.poll(wgpu::PollType::Wait {
                            submission_index: None,
                            timeout: None,
                        });
                    }
                })
                .expect("GPU completion thread");
            Self { sender }
        }
        #[cfg(target_family = "wasm")]
        {
            let _ = device;
            Self {}
        }
    }

    fn wake(&self) {
        #[cfg(not(target_family = "wasm"))]
        let _ = self.sender.try_send(());
    }
}

enum PendingUpload {
    Pixels {
        id: AtlasTextureId,
        bounds: Bounds<DevicePixels>,
        texture: wgpu::Texture,
        data: Vec<u8>,
        bytes_per_pixel: u8,
    },
    Clear(wgpu::Texture),
    #[cfg(target_os = "windows")]
    SharedTexture {
        id: AtlasTextureId,
        bounds: Bounds<DevicePixels>,
        texture: wgpu::Texture,
        source: ImportedTexture,
        origin: Point<DevicePixels>,
    },
    #[cfg(target_os = "linux")]
    Copy {
        id: AtlasTextureId,
        bounds: Bounds<DevicePixels>,
        texture: wgpu::Texture,
        source: Arc<ImportedBuffer>,
        origin: Point<DevicePixels>,
    },
}

impl PendingUpload {
    fn targets(&self, tile: AtlasTile) -> bool {
        match self {
            Self::Pixels { id, bounds, .. } => {
                *id == tile.texture_id && bounds.intersects(&tile.bounds)
            }
            Self::Clear(_) => false,
            #[cfg(target_os = "windows")]
            Self::SharedTexture { id, bounds, .. } => {
                *id == tile.texture_id && bounds.intersects(&tile.bounds)
            }
            #[cfg(target_os = "linux")]
            Self::Copy { id, bounds, .. } => {
                *id == tile.texture_id && bounds.intersects(&tile.bounds)
            }
        }
    }
}

struct WgpuAtlasTextures {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    max_texture_size: u32,
    color_texture_format: wgpu::TextureFormat,
    storage: WgpuAtlasStorage,
    pending_uploads: Vec<PendingUpload>,
    next_texture_generation: u64,
    completions: Vec<Box<dyn FnOnce() + Send>>,
    poller: Option<Arc<CompletionPoller>>,
    #[cfg(target_os = "linux")]
    dmabufs: FxHashMap<DmabufId, Arc<ImportedBuffer>>,
    #[cfg(target_os = "linux")]
    next_dmabuf: u64,
}

pub struct WgpuTextureInfo {
    pub view: wgpu::TextureView,
    /// Distinguishes new textures that reuse an [`AtlasTextureId`].
    pub generation: u64,
}

impl WgpuAtlas {
    pub fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        color_texture_format: wgpu::TextureFormat,
    ) -> Self {
        let max_texture_size = device.limits().max_texture_dimension_2d;
        WgpuAtlas(Arc::new(Mutex::new(AtlasState::new(WgpuAtlasTextures {
            device,
            queue,
            max_texture_size,
            color_texture_format,
            storage: WgpuAtlasStorage::default(),
            pending_uploads: Vec::new(),
            next_texture_generation: 0,
            completions: Vec::new(),
            poller: None,
            #[cfg(target_os = "linux")]
            dmabufs: FxHashMap::default(),
            #[cfg(target_os = "linux")]
            next_dmabuf: 1,
        }))))
    }

    pub fn from_context(context: &WgpuContext) -> Self {
        Self::new(
            context.device.clone(),
            context.queue.clone(),
            context.color_texture_format(),
        )
    }

    pub fn before_frame(&self) {
        let (device, queue, updates, completions, poller) = {
            let mut lock = self.0.lock();
            let backend = &mut lock.backend;
            (
                backend.device.clone(),
                backend.queue.clone(),
                std::mem::take(&mut backend.pending_uploads),
                std::mem::take(&mut backend.completions),
                backend.poller.clone(),
            )
        };
        if updates.is_empty() && completions.is_empty() {
            let _ = device.poll(wgpu::PollType::Poll);
            return;
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        let mut sources = Vec::new();
        for update in updates {
            match update {
                PendingUpload::Pixels {
                    bounds,
                    texture,
                    data,
                    bytes_per_pixel,
                    ..
                } => {
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: bounds.origin.x.0 as u32,
                                y: bounds.origin.y.0 as u32,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &data,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(
                                bounds.size.width.0 as u32 * bytes_per_pixel as u32,
                            ),
                            rows_per_image: None,
                        },
                        wgpu::Extent3d {
                            width: bounds.size.width.0 as u32,
                            height: bounds.size.height.0 as u32,
                            depth_or_array_layers: 1,
                        },
                    );
                }
                PendingUpload::Clear(texture) => {
                    let mut encoder =
                        device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("initialize atlas"),
                        });
                    encoder.clear_texture(&texture, &wgpu::ImageSubresourceRange::default());
                    queue.submit([encoder.finish()]);
                }
                #[cfg(target_os = "windows")]
                PendingUpload::SharedTexture {
                    texture,
                    source,
                    origin,
                    bounds,
                    ..
                } => {
                    if let Err(error) = source.copy_to(&device, &queue, &texture, origin, bounds) {
                        log::error!("shared texture copy failed: {error:#}");
                    }
                    sources.push(source);
                }
                #[cfg(target_os = "linux")]
                PendingUpload::Copy {
                    texture,
                    source,
                    origin,
                    bounds,
                    ..
                } => {
                    let mut encoder =
                        device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("browser buffer copy"),
                        });
                    match source.copy_to(&mut encoder, &texture, origin, bounds) {
                        Ok(()) => {
                            queue.submit([encoder.finish()]);
                        }
                        Err(error) => log::error!("DMA-BUF copy failed: {error:#}"),
                    }
                    sources.push(source);
                }
            }
        }
        queue.submit([]);
        queue.on_submitted_work_done(move || {
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            drop(sources);
            for complete in completions {
                complete();
            }
        });
        if let Some(poller) = poller {
            poller.wake();
        }
    }

    /// Returns the view backing `id`, or `None` once every tile in it has been
    /// removed. A scene can still reference such a texture when a cached view
    /// replays a paint from before the image was dropped, so callers must skip
    /// those sprites rather than assume the texture exists.
    pub fn get_texture_info(&self, id: AtlasTextureId) -> Option<WgpuTextureInfo> {
        let lock = self.0.lock();
        let texture = lock.backend.storage.get(id)?;
        Some(WgpuTextureInfo {
            view: texture.view.clone(),
            generation: texture.generation,
        })
    }

    /// Clears all cached textures and tiles, forcing them to be recreated.
    /// Use this for incremental recovery when the device is still valid.
    pub fn clear(&self) {
        let (queue, completions, poller) = {
            let mut lock = self.0.lock();
            let completions = std::mem::take(&mut lock.backend.completions);
            let queue = lock.backend.queue.clone();
            let poller = lock.backend.poller.clone();
            lock.clear(|backend| {
                backend.storage = WgpuAtlasStorage::default();
                backend.pending_uploads.clear();
            });
            (queue, completions, poller)
        };
        queue.on_submitted_work_done(move || {
            for complete in completions {
                complete();
            }
        });
        if let Some(poller) = poller {
            poller.wake();
        }
    }

    pub fn handle_device_lost(&self, context: &WgpuContext) {
        let (queue, completions, poller) = {
            let mut lock = self.0.lock();
            let completions = std::mem::take(&mut lock.backend.completions);
            let queue = lock.backend.queue.clone();
            let poller = lock.backend.poller.clone();
            lock.clear(|backend| {
                backend.poller = None;
                backend.device = context.device.clone();
                backend.queue = context.queue.clone();
                backend.color_texture_format = context.color_texture_format();
                backend.max_texture_size = context.device.limits().max_texture_dimension_2d;
                backend.storage = WgpuAtlasStorage::default();
                backend.pending_uploads.clear();
                #[cfg(target_os = "linux")]
                backend.dmabufs.clear();
            });
            (queue, completions, poller)
        };
        queue.on_submitted_work_done(move || {
            for complete in completions {
                complete();
            }
        });
        if let Some(poller) = poller {
            poller.wake();
        }
    }
}

impl PlatformAtlas for WgpuAtlas {
    fn get_or_insert_with<'a>(
        &self,
        key: AtlasKey,
        build: &mut dyn FnMut() -> Result<Option<(Size<DevicePixels>, Cow<'a, [u8]>)>>,
    ) -> Result<Option<AtlasTile>> {
        self.0.lock().get_or_insert_with(key, build)
    }

    fn remove(&self, key: &AtlasKey) {
        self.0.lock().remove(key);
    }
    fn insert_blank(&self, key: &AtlasKey, size: Size<DevicePixels>) -> Result<Option<bool>> {
        let mut lock = self.0.lock();
        if !lock
            .backend
            .device
            .features()
            .contains(wgpu::Features::CLEAR_TEXTURE)
        {
            return Ok(None);
        }
        if lock.contains(key) {
            return Ok(Some(false));
        }
        ensure!(
            size.width.0 > 0 && size.height.0 > 0,
            "invalid external image size"
        );
        let tile = lock
            .backend
            .allocate(size, key.texture_kind())
            .context("cannot allocate external image")?;
        lock.tiles_by_key.insert(key.clone(), tile);
        Ok(Some(true))
    }

    fn update_tile(
        &self,
        key: &AtlasKey,
        region: Bounds<DevicePixels>,
        update: AtlasTileUpdate<'_>,
        complete: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<bool> {
        let mut lock = self.0.lock();
        if lock.backend.poller.is_none() {
            lock.backend.poller =
                Some(Arc::new(CompletionPoller::new(lock.backend.device.clone())));
        }
        let tile = lock.tiles_by_key.get(key).copied();
        let result = match tile {
            Some(tile) => lock.backend.queue_update(tile, region, update),
            None => Ok(false),
        };
        lock.backend.completions.extend(complete);
        result
    }

    #[cfg(target_os = "windows")]
    fn shared_texture_adapter(&self) -> Option<gpui::external_image::DxgiAdapterId> {
        let lock = self.0.lock();
        let hal = unsafe { lock.backend.device.as_hal::<wgpu::hal::api::Dx12>() }?;
        let id = unsafe { hal.raw_device().GetAdapterLuid() };
        Some(gpui::external_image::DxgiAdapterId {
            low: id.LowPart,
            high: id.HighPart,
        })
    }

    #[cfg(target_os = "linux")]
    fn dmabuf_target(&self) -> Option<DmabufTarget> {
        let lock = self.0.lock();
        let backend = &lock.backend;
        if backend.color_texture_format != wgpu::TextureFormat::Bgra8Unorm
            || !backend
                .device
                .features()
                .contains(wgpu::Features::CLEAR_TEXTURE)
        {
            return None;
        }
        Some(DmabufTarget {
            render_node: backend.device.render_node()?,
            modifiers: backend.device.importable_modifiers(),
        })
    }

    #[cfg(target_os = "linux")]
    fn import_dmabuf(&self, buffer: &Dmabuf<'_>) -> Result<DmabufImport> {
        let device = {
            let lock = self.0.lock();
            ensure!(
                lock.backend.color_texture_format == wgpu::TextureFormat::Bgra8Unorm,
                "DMA-BUF requires a BGRA atlas"
            );
            lock.backend.device.clone()
        };
        ensure!(
            buffer.fourcc == u32::from_le_bytes(*b"AR24"),
            "unsupported DMA-BUF format"
        );
        let source = Arc::new(dmabuf::import(device.clone(), buffer)?);
        let mut lock = self.0.lock();
        ensure!(
            Arc::ptr_eq(&lock.backend.device, &device),
            "device changed during DMA-BUF import"
        );
        let id = DmabufId(lock.backend.next_dmabuf);
        lock.backend.next_dmabuf = lock
            .backend
            .next_dmabuf
            .checked_add(1)
            .context("DMA-BUF identifiers exhausted")?;
        lock.backend.dmabufs.insert(id, source);
        let state = Arc::downgrade(&self.0);
        Ok(DmabufImport::new(id, move || {
            if let Some(state) = state.upgrade() {
                state.lock().backend.dmabufs.remove(&id);
            }
        }))
    }
}

impl AtlasBackend for WgpuAtlasTextures {
    fn insert(
        &mut self,
        kind: AtlasTextureKind,
        size: Size<DevicePixels>,
        bytes: &[u8],
    ) -> Result<AtlasTile> {
        let tile = self.allocate(size, kind).context("failed to allocate")?;
        self.upload_texture(tile.texture_id, tile.bounds, bytes);
        Ok(tile)
    }

    fn remove(&mut self, tile: AtlasTile) {
        self.pending_uploads.retain(|upload| !upload.targets(tile));
        let id = tile.texture_id;
        let Some(texture_slot) = self.storage[id.kind].textures.get_mut(id.index as usize) else {
            return;
        };

        if let Some(mut texture) = texture_slot.take() {
            texture.allocator.deallocate(tile.tile_id.into());
            texture.decrement_ref_count();
            if texture.is_unreferenced() {
                self.storage[id.kind]
                    .free_list
                    .push(texture.id.index as usize);
            } else {
                *texture_slot = Some(texture);
            }
        }
    }
}

impl WgpuAtlasTextures {
    fn allocate(
        &mut self,
        size: Size<DevicePixels>,
        texture_kind: AtlasTextureKind,
    ) -> Option<AtlasTile> {
        {
            let textures = &mut self.storage[texture_kind];

            if let Some(tile) = textures
                .iter_mut()
                .rev()
                .find_map(|texture| texture.allocate(size))
            {
                return Some(tile);
            }
        }

        let texture = self.push_texture(size, texture_kind);
        texture.allocate(size)
    }

    fn push_texture(
        &mut self,
        min_size: Size<DevicePixels>,
        kind: AtlasTextureKind,
    ) -> &mut WgpuAtlasTexture {
        const DEFAULT_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(1024),
            height: DevicePixels(1024),
        };
        let max_texture_size = self.max_texture_size as i32;
        let max_atlas_size = Size {
            width: DevicePixels(max_texture_size),
            height: DevicePixels(max_texture_size),
        };

        let size = min_size.min(&max_atlas_size).max(&DEFAULT_ATLAS_SIZE);
        let format = match kind {
            AtlasTextureKind::Monochrome => wgpu::TextureFormat::R8Unorm,
            AtlasTextureKind::Subpixel | AtlasTextureKind::Polychrome => self.color_texture_format,
        };

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: wgpu::Extent3d {
                width: size.width.0 as u32,
                height: size.height.0 as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        if self
            .device
            .features()
            .contains(wgpu::Features::CLEAR_TEXTURE)
        {
            self.pending_uploads
                .push(PendingUpload::Clear(texture.clone()));
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let texture_list = &mut self.storage[kind];
        let index = texture_list.free_list.pop();
        let generation = self.next_texture_generation;
        self.next_texture_generation = self.next_texture_generation.wrapping_add(1);

        let atlas_texture = WgpuAtlasTexture {
            id: AtlasTextureId {
                index: index.unwrap_or(texture_list.textures.len()) as u32,
                kind,
            },
            generation,
            allocator: BucketedAtlasAllocator::new(device_size_to_etagere(size)),
            format,
            texture,
            view,
            live_atlas_keys: 0,
        };

        if let Some(ix) = index {
            texture_list.textures[ix] = Some(atlas_texture);
            texture_list
                .textures
                .get_mut(ix)
                .and_then(|t| t.as_mut())
                .expect("texture must exist")
        } else {
            texture_list.textures.push(Some(atlas_texture));
            texture_list
                .textures
                .last_mut()
                .and_then(|t| t.as_mut())
                .expect("texture must exist")
        }
    }

    fn upload_texture(&mut self, id: AtlasTextureId, bounds: Bounds<DevicePixels>, bytes: &[u8]) {
        if let Some(texture) = self.storage.get(id) {
            self.pending_uploads.push(PendingUpload::Pixels {
                id,
                bounds,
                data: swizzle_upload_data(bytes, texture.format),
                texture: texture.texture.clone(),
                bytes_per_pixel: texture.bytes_per_pixel(),
            });
        }
    }

    fn queue_update(
        &mut self,
        tile: AtlasTile,
        region: Bounds<DevicePixels>,
        update: AtlasTileUpdate<'_>,
    ) -> Result<bool> {
        let Some(texture) = self.storage.get(tile.texture_id) else {
            return Ok(false);
        };
        let size = tile.bounds.size;
        ensure!(
            region.origin.x.0 >= 0
                && region.origin.y.0 >= 0
                && region.size.width.0 > 0
                && region.size.height.0 > 0
                && i64::from(region.origin.x.0) + i64::from(region.size.width.0)
                    <= i64::from(size.width.0)
                && i64::from(region.origin.y.0) + i64::from(region.size.height.0)
                    <= i64::from(size.height.0),
            "external image update lies outside its tile"
        );
        ensure!(
            matches!(
                texture.format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            ),
            "external image requires a color atlas"
        );
        let bounds = Bounds::new(tile.bounds.origin + region.origin, region.size);
        match update {
            AtlasTileUpdate::Pixels {
                bytes,
                bytes_per_row,
            } => {
                let row = region.size.width.0 as usize * 4;
                let rows = region.size.height.0 as usize;
                let stride = bytes_per_row as usize;
                let length = stride
                    .checked_mul(rows - 1)
                    .and_then(|n| n.checked_add(row))
                    .context("external image byte length overflow")?;
                ensure!(
                    stride >= row && bytes.len() >= length,
                    "external image rows are incomplete"
                );
                let mut data = Vec::with_capacity(row * rows);
                for line in bytes.chunks(stride).take(rows) {
                    data.extend_from_slice(&line[..row]);
                }
                self.upload_texture(tile.texture_id, bounds, &data);
            }
            #[cfg(target_os = "windows")]
            AtlasTileUpdate::SharedTexture {
                texture: source,
                origin,
            } => {
                ensure!(
                    texture.format == wgpu::TextureFormat::Bgra8Unorm,
                    "shared D3D textures require a BGRA atlas"
                );
                let source = ImportedTexture::open(&self.device, source)?;
                ensure!(
                    source.contains(origin, region.size),
                    "copy lies outside the shared texture"
                );
                self.pending_uploads.push(PendingUpload::SharedTexture {
                    id: tile.texture_id,
                    bounds,
                    texture: texture.texture.clone(),
                    source,
                    origin,
                });
            }
            #[cfg(target_os = "linux")]
            AtlasTileUpdate::Dmabuf { buffer, origin } => {
                ensure!(
                    texture.format == wgpu::TextureFormat::Bgra8Unorm,
                    "DMA-BUF requires a BGRA atlas"
                );
                let source = self
                    .dmabufs
                    .get(&buffer)
                    .context("DMA-BUF import no longer exists")?;
                ensure!(
                    origin.x.0 >= 0
                        && origin.y.0 >= 0
                        && i64::from(origin.x.0) + i64::from(region.size.width.0)
                            <= i64::from(source.size.width)
                        && i64::from(origin.y.0) + i64::from(region.size.height.0)
                            <= i64::from(source.size.height),
                    "external image update lies outside its DMA-BUF"
                );
                self.pending_uploads.push(PendingUpload::Copy {
                    id: tile.texture_id,
                    bounds,
                    texture: texture.texture.clone(),
                    source: source.clone(),
                    origin,
                });
            }
        }
        Ok(true)
    }
}

#[derive(Default)]
struct WgpuAtlasStorage {
    monochrome_textures: AtlasTextureList<WgpuAtlasTexture>,
    subpixel_textures: AtlasTextureList<WgpuAtlasTexture>,
    polychrome_textures: AtlasTextureList<WgpuAtlasTexture>,
}

impl ops::Index<AtlasTextureKind> for WgpuAtlasStorage {
    type Output = AtlasTextureList<WgpuAtlasTexture>;
    fn index(&self, kind: AtlasTextureKind) -> &Self::Output {
        match kind {
            AtlasTextureKind::Monochrome => &self.monochrome_textures,
            AtlasTextureKind::Subpixel => &self.subpixel_textures,
            AtlasTextureKind::Polychrome => &self.polychrome_textures,
        }
    }
}

impl ops::IndexMut<AtlasTextureKind> for WgpuAtlasStorage {
    fn index_mut(&mut self, kind: AtlasTextureKind) -> &mut Self::Output {
        match kind {
            AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
            AtlasTextureKind::Subpixel => &mut self.subpixel_textures,
            AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
        }
    }
}

impl WgpuAtlasStorage {
    fn get(&self, id: AtlasTextureId) -> Option<&WgpuAtlasTexture> {
        self[id.kind]
            .textures
            .get(id.index as usize)
            .and_then(|t| t.as_ref())
    }
}

struct WgpuAtlasTexture {
    id: AtlasTextureId,
    generation: u64,
    allocator: BucketedAtlasAllocator,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    format: wgpu::TextureFormat,
    live_atlas_keys: u32,
}

impl WgpuAtlasTexture {
    fn allocate(&mut self, size: Size<DevicePixels>) -> Option<AtlasTile> {
        let allocation = self.allocator.allocate(device_size_to_etagere(size))?;
        let tile = AtlasTile {
            texture_id: self.id,
            tile_id: allocation.id.into(),
            padding: 0,
            bounds: Bounds {
                origin: etagere_point_to_device(allocation.rectangle.min),
                size,
            },
        };
        self.live_atlas_keys += 1;
        Some(tile)
    }

    fn bytes_per_pixel(&self) -> u8 {
        match self.format {
            wgpu::TextureFormat::R8Unorm => 1,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm => 4,
            _ => 4,
        }
    }

    fn decrement_ref_count(&mut self) {
        self.live_atlas_keys -= 1;
    }

    fn is_unreferenced(&self) -> bool {
        self.live_atlas_keys == 0
    }
}

fn swizzle_upload_data(bytes: &[u8], format: wgpu::TextureFormat) -> Vec<u8> {
    match format {
        wgpu::TextureFormat::Rgba8Unorm => {
            let mut data = bytes.to_vec();
            for pixel in data.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            data
        }
        _ => bytes.to_vec(),
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;
    use gpui::{ImageId, RenderImageParams, block_on};
    use std::sync::Arc;

    fn test_device_and_queue() -> anyhow::Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
        block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::all(),
                flags: wgpu::InstanceFlags::default(),
                backend_options: wgpu::BackendOptions::default(),
                memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                display: None,
            });
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                    apply_limit_buckets: false,
                })
                .await
                .map_err(|error| anyhow::anyhow!("failed to request adapter: {error}"))?;
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("wgpu_atlas_test_device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults()
                        .using_resolution(adapter.limits())
                        .using_alignment(adapter.limits()),
                    memory_hints: wgpu::MemoryHints::MemoryUsage,
                    trace: wgpu::Trace::Off,
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                })
                .await
                .map_err(|error| anyhow::anyhow!("failed to request device: {error}"))?;
            Ok((Arc::new(device), Arc::new(queue)))
        })
    }

    #[test]
    fn before_frame_skips_uploads_for_removed_texture() -> anyhow::Result<()> {
        let (device, queue) = test_device_and_queue()?;

        let atlas = WgpuAtlas::new(device, queue, wgpu::TextureFormat::Bgra8Unorm);
        let key = AtlasKey::Image(RenderImageParams {
            image_id: ImageId(1),
            frame_index: 0,
        });
        let size = Size {
            width: DevicePixels(1),
            height: DevicePixels(1),
        };
        let mut build = || Ok(Some((size, Cow::Owned(vec![0, 0, 0, 255]))));

        // Regression test: before the fix, this panicked in flush_uploads
        atlas
            .get_or_insert_with(key.clone(), &mut build)?
            .expect("tile should be created");
        atlas.remove(&key);
        atlas.before_frame();
        Ok(())
    }

    #[test]
    fn remove_deallocates_tile_space_for_reuse() -> anyhow::Result<()> {
        let (device, queue) = test_device_and_queue()?;
        let atlas = WgpuAtlas::new(device, queue, wgpu::TextureFormat::Bgra8Unorm);

        let small = Size {
            width: DevicePixels(64),
            height: DevicePixels(64),
        };
        let big = Size {
            width: DevicePixels(700),
            height: DevicePixels(700),
        };

        let make_key = |image_id: usize| {
            AtlasKey::Image(RenderImageParams {
                image_id: ImageId(image_id),
                frame_index: 0,
            })
        };
        let insert = |key: AtlasKey, size: Size<DevicePixels>| {
            let byte_count = (size.width.0 as usize) * (size.height.0 as usize) * 4;
            atlas
                .get_or_insert_with(key, &mut || {
                    Ok(Some((size, Cow::Owned(vec![0u8; byte_count]))))
                })
                .expect("allocation should succeed")
                .expect("callback returns Some")
        };

        let keeper_key = make_key(1);
        let big_key_a = make_key(2);
        let big_key_b = make_key(3);

        let keeper_tile = insert(keeper_key, small);
        let tile_a = insert(big_key_a.clone(), big);
        assert_eq!(keeper_tile.texture_id, tile_a.texture_id);

        atlas.remove(&big_key_a);
        let tile_b = insert(big_key_b, big);
        assert_eq!(tile_b.texture_id, keeper_tile.texture_id);
        Ok(())
    }

    #[test]
    fn reused_texture_id_has_new_generation() -> anyhow::Result<()> {
        let (device, queue) = test_device_and_queue()?;
        let atlas = WgpuAtlas::new(device, queue, wgpu::TextureFormat::Bgra8Unorm);
        let size = Size {
            width: DevicePixels(700),
            height: DevicePixels(700),
        };
        let make_key = |image_id| {
            AtlasKey::Image(RenderImageParams {
                image_id: ImageId(image_id),
                frame_index: 0,
            })
        };
        let insert = |key: AtlasKey| {
            atlas
                .get_or_insert_with(key, &mut || {
                    Ok(Some((
                        size,
                        Cow::Owned(vec![0; size.width.0 as usize * size.height.0 as usize * 4]),
                    )))
                })
                .expect("allocation should succeed")
                .expect("callback returns Some")
        };

        let first_key = make_key(1);
        let first_tile = insert(first_key.clone());
        let first_generation = atlas
            .get_texture_info(first_tile.texture_id)
            .context("first texture should exist")?
            .generation;
        atlas.remove(&first_key);
        assert!(atlas.get_texture_info(first_tile.texture_id).is_none());

        let second_tile = insert(make_key(2));
        let second_generation = atlas
            .get_texture_info(second_tile.texture_id)
            .context("second texture should exist")?
            .generation;

        assert_eq!(second_tile.texture_id, first_tile.texture_id);
        assert_ne!(second_generation, first_generation);
        Ok(())
    }

    #[test]
    fn swizzle_upload_data_preserves_bgra_uploads() {
        let input = vec![0x10, 0x20, 0x30, 0x40];
        assert_eq!(
            swizzle_upload_data(&input, wgpu::TextureFormat::Bgra8Unorm),
            input
        );
    }

    #[test]
    fn swizzle_upload_data_converts_bgra_to_rgba() {
        let input = vec![0x10, 0x20, 0x30, 0x40, 0xAA, 0xBB, 0xCC, 0xDD];
        assert_eq!(
            swizzle_upload_data(&input, wgpu::TextureFormat::Rgba8Unorm),
            vec![0x30, 0x20, 0x10, 0x40, 0xCC, 0xBB, 0xAA, 0xDD]
        );
    }
}

impl Drop for WgpuAtlasTextures {
    fn drop(&mut self) {
        let completions = std::mem::take(&mut self.completions);
        self.queue.on_submitted_work_done(move || {
            for complete in completions {
                complete();
            }
        });
        if let Some(poller) = &self.poller {
            poller.wake();
        }
    }
}
