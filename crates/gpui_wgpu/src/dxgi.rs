use anyhow::{Context as _, Result, ensure};
use gpui::{Bounds, DevicePixels, Point, external_image::SharedTexture};
use std::os::windows::io::AsRawHandle as _;
use wgpu::hal::api::Dx12;
use windows::Win32::{
    Foundation::HANDLE,
    Graphics::{
        Direct3D12::{self, ID3D12Fence, ID3D12Resource},
        Dxgi::Common,
    },
};

pub(crate) struct ImportedTexture {
    texture: wgpu::Texture,
    fence: ID3D12Fence,
    value: u64,
}

impl ImportedTexture {
    pub(crate) fn open(device: &wgpu::Device, source: SharedTexture<'_>) -> Result<Self> {
        let hal = unsafe { device.as_hal::<Dx12>() }
            .context("shared D3D textures require the DX12 renderer")?;
        let raw = hal.raw_device();
        let mut resource: Option<ID3D12Resource> = None;
        let mut fence: Option<ID3D12Fence> = None;
        // SAFETY: borrowed NT handles remain open throughout both imports.
        unsafe {
            raw.OpenSharedHandle(HANDLE(source.resource.as_raw_handle()), &mut resource)?;
            raw.OpenSharedHandle(HANDLE(source.fence.as_raw_handle()), &mut fence)?;
        }
        let resource = resource.context("shared texture import returned no resource")?;
        let fence = fence.context("shared fence import returned no fence")?;
        let desc = unsafe { resource.GetDesc() };
        ensure!(
            desc.Dimension == Direct3D12::D3D12_RESOURCE_DIMENSION_TEXTURE2D
                && desc.Format == Common::DXGI_FORMAT_B8G8R8A8_UNORM
                && desc.Width > 0
                && desc.Height > 0
                && desc.Width <= u64::from(device.limits().max_texture_dimension_2d)
                && desc.Height <= device.limits().max_texture_dimension_2d
                && desc.DepthOrArraySize == 1
                && desc.MipLevels == 1
                && desc.SampleDesc.Count == 1,
            "shared texture must be single-sample BGRA8 with one layer and mip"
        );
        let size = wgpu::Extent3d {
            width: desc.Width as u32,
            height: desc.Height,
            depth_or_array_layers: 1,
        };
        // SAFETY: the resource was opened on this device; its native descriptor
        // matches the wrapper. The producer contract requires COMMON state.
        let texture = unsafe {
            let texture = wgpu::hal::dx12::Device::texture_from_raw(
                resource,
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureDimension::D2,
                size,
                1,
                1,
            );
            device.create_texture_from_hal::<Dx12>(
                texture,
                &wgpu::TextureDescriptor {
                    label: Some("shared browser texture"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    usage: wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                },
                wgpu::TextureUses::PRESENT,
            )
        };
        Ok(Self {
            texture,
            fence,
            value: source.value,
        })
    }

    pub(crate) fn contains(
        &self,
        origin: Point<DevicePixels>,
        size: gpui::Size<DevicePixels>,
    ) -> bool {
        origin.x.0 >= 0
            && origin.y.0 >= 0
            && size.width.0 > 0
            && size.height.0 > 0
            && i64::from(origin.x.0) + i64::from(size.width.0) <= i64::from(self.texture.width())
            && i64::from(origin.y.0) + i64::from(size.height.0) <= i64::from(self.texture.height())
    }

    pub(crate) fn copy_to(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::Texture,
        origin: Point<DevicePixels>,
        bounds: Bounds<DevicePixels>,
    ) -> Result<()> {
        let hal = unsafe { device.as_hal::<Dx12>() }.context("DX12 device was lost")?;
        // SAFETY: both resources were opened by this device. This is a GPU wait;
        // the UI thread does not wait for the producer fence.
        unsafe {
            hal.raw_queue().Wait(&self.fence, self.value)?;
        }
        drop(hal);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("shared browser texture copy"),
        });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: origin.x.0 as u32,
                    y: origin.y.0 as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: bounds.origin.x.0 as u32,
                    y: bounds.origin.y.0 as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: bounds.size.width.0 as u32,
                height: bounds.size.height.0 as u32,
                depth_or_array_layers: 1,
            },
        );
        encoder.transition_resources(
            std::iter::empty(),
            std::iter::once(wgpu::TextureTransition {
                texture: &self.texture,
                selector: None,
                state: wgpu::TextureUses::PRESENT,
            }),
        );
        queue.submit([encoder.finish()]);
        Ok(())
    }
}
