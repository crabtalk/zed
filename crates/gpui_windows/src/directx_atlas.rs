use anyhow::{Context as _, Result, ensure};
use etagere::BucketedAtlasAllocator;
use gpui::{
    AtlasBackend, AtlasKey, AtlasState, AtlasTextureId, AtlasTextureKind, AtlasTextureList,
    AtlasTile, Bounds, DevicePixels, PlatformAtlas, Point, Size,
    external_image::{AtlasTileUpdate, DxgiAdapterId},
};
use parking_lot::Mutex;
use std::{os::windows::io::AsRawHandle as _, sync::mpsc, thread, time::Duration};
use windows::{
    Win32::{
        Foundation::HANDLE,
        Graphics::{
            Direct3D11::{
                self, D3D11_BOX, D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11Device5,
                ID3D11DeviceContext, ID3D11DeviceContext4, ID3D11Fence, ID3D11ShaderResourceView,
                ID3D11Texture2D,
            },
            Dxgi::{
                Common::{self, DXGI_SAMPLE_DESC},
                IDXGIDevice,
            },
        },
    },
    core::Interface as _,
};

pub(crate) struct DirectXAtlas(Mutex<AtlasState<DirectXAtlasTextures>>);

struct DirectXAtlasTextures {
    device: ID3D11Device,
    device_context: ID3D11DeviceContext,
    completion: Option<Completion>,
    monochrome_textures: AtlasTextureList<DirectXAtlasTexture>,
    polychrome_textures: AtlasTextureList<DirectXAtlasTexture>,
    subpixel_textures: AtlasTextureList<DirectXAtlasTexture>,
}

struct DirectXAtlasTexture {
    id: AtlasTextureId,
    bytes_per_pixel: u32,
    allocator: BucketedAtlasAllocator,
    texture: ID3D11Texture2D,
    view: [Option<ID3D11ShaderResourceView>; 1],
    live_atlas_keys: u32,
}

impl DirectXAtlas {
    pub(crate) fn new(device: &ID3D11Device, device_context: &ID3D11DeviceContext) -> Self {
        DirectXAtlas(Mutex::new(AtlasState::new(DirectXAtlasTextures {
            device: device.clone(),
            device_context: device_context.clone(),
            completion: None,
            monochrome_textures: Default::default(),
            polychrome_textures: Default::default(),
            subpixel_textures: Default::default(),
        })))
    }

    /// Returns the view backing `id`, or `None` once every tile in it has been
    /// removed. A scene can still reference such a texture when a cached view
    /// replays a paint from before the image was dropped, so callers must skip
    /// those sprites rather than assume the texture exists.
    pub(crate) fn get_texture_view(
        &self,
        id: AtlasTextureId,
    ) -> Option<[Option<ID3D11ShaderResourceView>; 1]> {
        let lock = self.0.lock();
        let texture = lock.backend.texture(id)?;
        Some(texture.view.clone())
    }

    pub(crate) fn handle_device_lost(
        &self,
        device: &ID3D11Device,
        device_context: &ID3D11DeviceContext,
    ) {
        let mut lock = self.0.lock();
        lock.clear(|textures| {
            textures.device = device.clone();
            textures.device_context = device_context.clone();
            textures.completion = None;
            textures.monochrome_textures = AtlasTextureList::default();
            textures.polychrome_textures = AtlasTextureList::default();
            textures.subpixel_textures = AtlasTextureList::default();
        });
    }
}

impl PlatformAtlas for DirectXAtlas {
    fn get_or_insert_with<'a>(
        &self,
        key: AtlasKey,
        build: &mut dyn FnMut() -> anyhow::Result<
            Option<(Size<DevicePixels>, std::borrow::Cow<'a, [u8]>)>,
        >,
    ) -> anyhow::Result<Option<AtlasTile>> {
        self.0.lock().get_or_insert_with(key, build)
    }

    fn remove(&self, key: &AtlasKey) {
        self.0.lock().remove(key);
    }
    fn shared_texture_adapter(&self) -> Option<DxgiAdapterId> {
        let lock = self.0.lock();
        lock.backend.device.cast::<ID3D11Device5>().ok()?;
        lock.backend
            .device_context
            .cast::<ID3D11DeviceContext4>()
            .ok()?;
        let device: IDXGIDevice = lock.backend.device.cast().ok()?;
        let description = unsafe { device.GetAdapter().ok()?.GetDesc().ok()? };
        Some(DxgiAdapterId {
            low: description.AdapterLuid.LowPart,
            high: description.AdapterLuid.HighPart,
        })
    }

    fn update_tile(
        &self,
        key: &AtlasKey,
        region: Bounds<DevicePixels>,
        update: AtlasTileUpdate<'_>,
        complete: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<bool> {
        let mut lock = self.0.lock();
        if lock.backend.completion.is_none() {
            match Completion::new(&lock.backend.device, &lock.backend.device_context) {
                Ok(completion) => lock.backend.completion = Some(completion),
                Err(error) => {
                    drop(lock);
                    if let Some(complete) = complete {
                        complete();
                    }
                    return Err(error);
                }
            }
        }
        let mut retained = Vec::new();
        let result = (|| -> Result<bool> {
            let Some(tile) = lock.tiles_by_key.get(key).copied() else {
                return Ok(false);
            };
            ensure!(
                region.origin.x.0 >= 0
                    && region.origin.y.0 >= 0
                    && region.size.width.0 > 0
                    && region.size.height.0 > 0
                    && i64::from(region.origin.x.0) + i64::from(region.size.width.0)
                        <= i64::from(tile.bounds.size.width.0)
                    && i64::from(region.origin.y.0) + i64::from(region.size.height.0)
                        <= i64::from(tile.bounds.size.height.0),
                "external image update lies outside its tile"
            );
            let texture = lock
                .backend
                .texture(tile.texture_id)
                .context("external image tile was evicted")?;
            ensure!(
                texture.bytes_per_pixel == 4
                    && tile.texture_id.kind == AtlasTextureKind::Polychrome,
                "external image needs a BGRA atlas"
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
                        .and_then(|length| length.checked_add(row))
                        .context("external image size overflow")?;
                    ensure!(
                        stride >= row && bytes.len() >= length,
                        "external image rows are incomplete"
                    );
                    let mut data = Vec::with_capacity(row * rows);
                    for line in bytes.chunks(stride).take(rows) {
                        data.extend_from_slice(&line[..row]);
                    }
                    texture.upload(&lock.backend.device_context, bounds, &data);
                }
                AtlasTileUpdate::SharedTexture {
                    texture: source,
                    origin,
                } => {
                    let device: ID3D11Device5 = lock.backend.device.cast()?;
                    let context: ID3D11DeviceContext4 = lock.backend.device_context.cast()?;
                    let source_texture: ID3D11Texture2D = unsafe {
                        device.OpenSharedResource1(HANDLE(source.resource.as_raw_handle()))?
                    };
                    let mut fence: Option<ID3D11Fence> = None;
                    unsafe {
                        device.OpenSharedFence(HANDLE(source.fence.as_raw_handle()), &mut fence)?;
                    }
                    let fence = fence.context("shared texture fence unavailable")?;
                    let mut description = D3D11_TEXTURE2D_DESC::default();
                    unsafe {
                        source_texture.GetDesc(&mut description);
                    }
                    ensure!(
                        description.Format == Common::DXGI_FORMAT_B8G8R8A8_UNORM
                            && description.ArraySize == 1
                            && description.MipLevels == 1
                            && description.SampleDesc.Count == 1,
                        "shared texture must be single-layer BGRA8"
                    );
                    ensure!(
                        origin.x.0 >= 0
                            && origin.y.0 >= 0
                            && i64::from(origin.x.0) + i64::from(region.size.width.0)
                                <= i64::from(description.Width)
                            && i64::from(origin.y.0) + i64::from(region.size.height.0)
                                <= i64::from(description.Height),
                        "copy lies outside shared texture"
                    );
                    let source_box = D3D11_BOX {
                        left: origin.x.0 as u32,
                        top: origin.y.0 as u32,
                        front: 0,
                        right: (origin.x.0 + region.size.width.0) as u32,
                        bottom: (origin.y.0 + region.size.height.0) as u32,
                        back: 1,
                    };
                    unsafe {
                        context.Wait(&fence, source.value)?;
                        context.CopySubresourceRegion(
                            &texture.texture,
                            0,
                            bounds.origin.x.0 as u32,
                            bounds.origin.y.0 as u32,
                            0,
                            &source_texture,
                            0,
                            Some(&source_box),
                        );
                    }
                    retained.push(source_texture);
                    retained.push(texture.texture.clone());
                }
            }
            Ok(true)
        })();
        let completion = lock
            .backend
            .completion
            .as_mut()
            .context("GPU completion queue unavailable")?;
        let submitted = completion.retire(retained, complete);
        result.and_then(|updated| submitted.map(|()| updated))
    }
}

impl AtlasBackend for DirectXAtlasTextures {
    fn insert(
        &mut self,
        kind: AtlasTextureKind,
        size: Size<DevicePixels>,
        bytes: &[u8],
    ) -> anyhow::Result<AtlasTile> {
        let tile = self
            .allocate(size, kind)
            .ok_or_else(|| anyhow::anyhow!("failed to allocate"))?;
        let texture = self
            .texture(tile.texture_id)
            .ok_or_else(|| anyhow::anyhow!("allocated tile refers to a missing texture"))?;
        texture.upload(&self.device_context, tile.bounds, bytes);
        Ok(tile)
    }

    fn remove(&mut self, tile: AtlasTile) {
        let id = tile.texture_id;

        let textures = match id.kind {
            AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
            AtlasTextureKind::Subpixel => &mut self.subpixel_textures,
        };

        let Some(texture_slot) = textures.textures.get_mut(id.index as usize) else {
            return;
        };

        if let Some(mut texture) = texture_slot.take() {
            texture.allocator.deallocate(tile.tile_id.into());
            texture.decrement_ref_count();
            if texture.is_unreferenced() {
                textures.free_list.push(texture.id.index as usize);
            } else {
                *texture_slot = Some(texture);
            }
        }
    }
}

impl DirectXAtlasTextures {
    fn allocate(
        &mut self,
        size: Size<DevicePixels>,
        texture_kind: AtlasTextureKind,
    ) -> Option<AtlasTile> {
        {
            let textures = match texture_kind {
                AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
                AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
                AtlasTextureKind::Subpixel => &mut self.subpixel_textures,
            };

            if let Some(tile) = textures
                .iter_mut()
                .rev()
                .find_map(|texture| texture.allocate(size))
            {
                return Some(tile);
            }
        }

        let texture = self.push_texture(size, texture_kind)?;
        texture.allocate(size)
    }

    fn push_texture(
        &mut self,
        min_size: Size<DevicePixels>,
        kind: AtlasTextureKind,
    ) -> Option<&mut DirectXAtlasTexture> {
        const DEFAULT_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(1024),
            height: DevicePixels(1024),
        };
        // Max texture size for DirectX. See:
        // https://learn.microsoft.com/en-us/windows/win32/direct3d11/overviews-direct3d-11-resources-limits
        const MAX_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(16384),
            height: DevicePixels(16384),
        };
        let size = min_size.min(&MAX_ATLAS_SIZE).max(&DEFAULT_ATLAS_SIZE);
        let pixel_format;
        let bind_flag;
        let bytes_per_pixel;
        match kind {
            AtlasTextureKind::Monochrome => {
                pixel_format = Common::DXGI_FORMAT_R8_UNORM;
                bind_flag = Direct3D11::D3D11_BIND_SHADER_RESOURCE;
                bytes_per_pixel = 1;
            }
            AtlasTextureKind::Polychrome => {
                pixel_format = Common::DXGI_FORMAT_B8G8R8A8_UNORM;
                bind_flag = Direct3D11::D3D11_BIND_SHADER_RESOURCE;
                bytes_per_pixel = 4;
            }
            AtlasTextureKind::Subpixel => {
                pixel_format = Common::DXGI_FORMAT_R8G8B8A8_UNORM;
                bind_flag = Direct3D11::D3D11_BIND_SHADER_RESOURCE;
                bytes_per_pixel = 4;
            }
        }
        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: size.width.0 as u32,
            Height: size.height.0 as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: pixel_format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: Direct3D11::D3D11_USAGE_DEFAULT,
            BindFlags: bind_flag.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        unsafe {
            // This only returns None if the device is lost, which we will recreate later.
            // So it's ok to return None here.
            self.device
                .CreateTexture2D(&texture_desc, None, Some(&mut texture))
                .ok()?;
        }
        let texture = texture.unwrap();

        let texture_list = match kind {
            AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
            AtlasTextureKind::Subpixel => &mut self.subpixel_textures,
        };
        let index = texture_list.free_list.pop();
        let view = unsafe {
            let mut view = None;
            self.device
                .CreateShaderResourceView(&texture, None, Some(&mut view))
                .ok()?;
            [view]
        };
        let atlas_texture = DirectXAtlasTexture {
            id: AtlasTextureId {
                index: index.unwrap_or(texture_list.textures.len()) as u32,
                kind,
            },
            bytes_per_pixel,
            allocator: etagere::BucketedAtlasAllocator::new(device_size_to_etagere(size)),
            texture,
            view,
            live_atlas_keys: 0,
        };
        if let Some(ix) = index {
            texture_list.textures[ix] = Some(atlas_texture);
            texture_list.textures.get_mut(ix).unwrap().as_mut()
        } else {
            texture_list.textures.push(Some(atlas_texture));
            texture_list.textures.last_mut().unwrap().as_mut()
        }
    }

    fn texture(&self, id: AtlasTextureId) -> Option<&DirectXAtlasTexture> {
        let textures = match id.kind {
            AtlasTextureKind::Monochrome => &self.monochrome_textures,
            AtlasTextureKind::Polychrome => &self.polychrome_textures,
            AtlasTextureKind::Subpixel => &self.subpixel_textures,
        };
        textures.textures.get(id.index as usize)?.as_ref()
    }
}

impl DirectXAtlasTexture {
    fn allocate(&mut self, size: Size<DevicePixels>) -> Option<AtlasTile> {
        let allocation = self.allocator.allocate(device_size_to_etagere(size))?;
        let tile = AtlasTile {
            texture_id: self.id,
            tile_id: allocation.id.into(),
            bounds: Bounds {
                origin: etagere_point_to_device(allocation.rectangle.min),
                size,
            },
            padding: 0,
        };
        self.live_atlas_keys += 1;
        Some(tile)
    }

    fn upload(
        &self,
        device_context: &ID3D11DeviceContext,
        bounds: Bounds<DevicePixels>,
        bytes: &[u8],
    ) {
        // `UpdateSubresource` reads `row_pitch * height` bytes from `bytes` based on the
        // `D3D11_BOX` below. If the caller hands us a slice shorter than that, the driver would
        // over-read past the end of the source buffer (potentially by multiple megabytes), so bail
        // out instead. This is a first-insert path rather than a per-frame one, so the check is
        // effectively free.
        let row_bytes = bounds.size.width.to_bytes(self.bytes_per_pixel as u8) as usize;
        let expected = row_bytes * bounds.size.height.0.max(0) as usize;
        if bytes.len() < expected {
            log::error!(
                "DirectXAtlasTexture::upload: source slice is {} bytes but the {}x{} region \
                 requires {} bytes; skipping upload to avoid a driver over-read",
                bytes.len(),
                bounds.size.width.0,
                bounds.size.height.0,
                expected,
            );
            return;
        }
        unsafe {
            device_context.UpdateSubresource(
                &self.texture,
                0,
                Some(&D3D11_BOX {
                    left: bounds.left().0 as u32,
                    top: bounds.top().0 as u32,
                    front: 0,
                    right: bounds.right().0 as u32,
                    bottom: bounds.bottom().0 as u32,
                    back: 1,
                }),
                bytes.as_ptr() as _,
                bounds.size.width.to_bytes(self.bytes_per_pixel as u8),
                0,
            );
        }
    }

    fn decrement_ref_count(&mut self) {
        self.live_atlas_keys -= 1;
    }

    fn is_unreferenced(&mut self) -> bool {
        self.live_atlas_keys == 0
    }
}

fn device_size_to_etagere(size: Size<DevicePixels>) -> etagere::Size {
    etagere::Size::new(size.width.into(), size.height.into())
}

fn etagere_point_to_device(value: etagere::Point) -> Point<DevicePixels> {
    Point {
        x: DevicePixels::from(value.x),
        y: DevicePixels::from(value.y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{ImageId, RenderImageParams};
    use std::borrow::Cow;
    use windows::Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_WARP,
            Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice},
        },
    };

    fn create_atlas() -> Option<DirectXAtlas> {
        let mut device: Option<ID3D11Device> = None;
        let mut device_context: Option<ID3D11DeviceContext> = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut device_context),
            )
        }
        .ok()?;
        Some(DirectXAtlas::new(&device?, &device_context?))
    }

    fn make_image_key(image_id: usize) -> AtlasKey {
        AtlasKey::Image(RenderImageParams {
            image_id: ImageId(image_id),
            frame_index: 0,
        })
    }

    fn insert_tile(atlas: &DirectXAtlas, key: AtlasKey, size: Size<DevicePixels>) -> AtlasTile {
        atlas
            .get_or_insert_with(key, &mut || {
                let byte_count = (size.width.0 as usize) * (size.height.0 as usize) * 4;
                Ok(Some((size, Cow::Owned(vec![0u8; byte_count]))))
            })
            .expect("allocation should succeed")
            .expect("callback returns Some")
    }

    #[test]
    fn test_remove_deallocates_tile_space_for_reuse() {
        let Some(atlas) = create_atlas() else {
            return;
        };

        let small = Size {
            width: DevicePixels(64),
            height: DevicePixels(64),
        };
        let big = Size {
            width: DevicePixels(700),
            height: DevicePixels(700),
        };

        let keeper_key = make_image_key(1);
        let big_key_a = make_image_key(2);
        let big_key_b = make_image_key(3);

        let keeper_tile = insert_tile(&atlas, keeper_key, small);
        let tile_a = insert_tile(&atlas, big_key_a.clone(), big);
        assert_eq!(keeper_tile.texture_id, tile_a.texture_id);

        atlas.remove(&big_key_a);

        let tile_b = insert_tile(&atlas, big_key_b, big);
        assert_eq!(tile_b.texture_id, keeper_tile.texture_id);
    }
}

struct Pending {
    value: u64,
    _textures: Vec<ID3D11Texture2D>,
    complete: Option<Box<dyn FnOnce() + Send>>,
}

struct Completion {
    fence: ID3D11Fence,
    context: ID3D11DeviceContext4,
    value: u64,
    sender: mpsc::Sender<Pending>,
}
impl Completion {
    fn new(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<Self> {
        let device5: ID3D11Device5 = device.cast()?;
        let context: ID3D11DeviceContext4 = context.cast()?;
        let mut fence: Option<ID3D11Fence> = None;
        unsafe {
            device5.CreateFence(0, Direct3D11::D3D11_FENCE_FLAG_NONE, &mut fence)?;
        }
        let fence = fence.context("GPU completion fence unavailable")?;
        let (sender, receiver) = mpsc::channel::<Pending>();
        let completed = fence.clone();
        let device = device.clone();
        thread::Builder::new()
            .name("gpui-d3d-completion".into())
            .spawn(move || {
                for pending in receiver {
                    loop {
                        let value = unsafe { completed.GetCompletedValue() };
                        if value >= pending.value
                            || unsafe { device.GetDeviceRemovedReason() }.is_err()
                        {
                            break;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    if let Some(complete) = pending.complete {
                        complete();
                    }
                }
            })?;
        Ok(Self {
            fence,
            context,
            value: 0,
            sender,
        })
    }
    fn retire(
        &mut self,
        textures: Vec<ID3D11Texture2D>,
        complete: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<()> {
        self.value = self
            .value
            .checked_add(1)
            .context("GPU completion fence exhausted")?;
        let signal = unsafe { self.context.Signal(&self.fence, self.value) };
        unsafe {
            self.context.Flush();
        }
        // Device removal also completes retirement. A signal error must not
        // return a producer's buffer while earlier copies may still read it.
        self.sender
            .send(Pending {
                value: self.value,
                _textures: textures,
                complete,
            })
            .map_err(|_| anyhow::anyhow!("GPU completion worker stopped"))?;
        signal?;
        Ok(())
    }
}
