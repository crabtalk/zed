//! Imports dma-bufs other processes render into as Vulkan images, linear or
//! tiled, so the atlas can copy their damaged regions into its textures on
//! the GPU.

use anyhow::{Context as _, Result, anyhow, ensure};
use ash::vk;
use gpui::{Bounds, DevicePixels, Point, external_image::Dmabuf};
use std::{
    io::{Seek as _, SeekFrom},
    os::fd::{AsRawFd as _, IntoRawFd as _},
    path::PathBuf,
    sync::Arc,
};
use wgpu::hal::api::Vulkan;

/// DRM's `AR24`, the only format the atlas imports: B, G, R, A bytes.
const FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;

/// Lets images take a dma-buf's layout, tiled ones included; wgpu leaves it off.
const MODIFIERS: &std::ffi::CStr = ash::ext::image_drm_format_modifier::NAME;

const IMPORT_EXTENSIONS: [&std::ffi::CStr; 4] = [
    ash::khr::external_memory_fd::NAME,
    ash::ext::external_memory_dma_buf::NAME,
    MODIFIERS,
    ash::ext::queue_family_foreign::NAME,
];

fn device_node(major: u64, minor: u64) -> Option<PathBuf> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    // Linux dev_t layout (see major()/minor() in sys/sysmacros.h).
    let split = |dev: u64| {
        (
            ((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0x0000_0fff),
            ((dev >> 12) & 0xffff_ff00) | (dev & 0x0000_00ff),
        )
    };
    std::fs::read_dir("/dev/dri")
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            std::fs::metadata(path).is_ok_and(|metadata| {
                metadata.file_type().is_char_device() && split(metadata.rdev()) == (major, minor)
            })
        })
}

fn layouts(hal: &wgpu::hal::vulkan::Device) -> Vec<(u64, vk::Extent3D)> {
    let extensions = hal.enabled_device_extensions();
    if !IMPORT_EXTENSIONS
        .iter()
        .all(|name| extensions.contains(name))
    {
        return Vec::new();
    }
    let instance = hal.shared_instance().raw_instance();
    let physical = hal.raw_physical_device();
    let query = |list: &mut vk::DrmFormatModifierPropertiesListEXT| {
        let mut properties = vk::FormatProperties2::default().push_next(list);
        unsafe {
            instance.get_physical_device_format_properties2(physical, FORMAT, &mut properties)
        };
    };
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
    query(&mut list);
    let mut layouts = vec![
        vk::DrmFormatModifierPropertiesEXT::default();
        list.drm_format_modifier_count as usize
    ];
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
        .drm_format_modifier_properties(&mut layouts);
    query(&mut list);
    let count = list.drm_format_modifier_count as usize;
    layouts.truncate(count);
    layouts
        .iter()
        .filter(|layout| {
            layout.drm_format_modifier_plane_count == 1
                && layout
                    .drm_format_modifier_tiling_features
                    .contains(vk::FormatFeatureFlags::TRANSFER_SRC)
        })
        .filter_map(|layout| {
            let modifier = layout.drm_format_modifier;
            Some((modifier, max_extent(instance, physical, modifier)?))
        })
        .collect()
}

/// The largest image in `modifier`'s layout the device imports from dma-bufs.
fn max_extent(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    modifier: u64,
) -> Option<vk::Extent3D> {
    let mut layout = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
        .drm_format_modifier(modifier)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let mut handle = vk::PhysicalDeviceExternalImageFormatInfo::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(FORMAT)
        .ty(vk::ImageType::TYPE_2D)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(vk::ImageUsageFlags::TRANSFER_SRC)
        .push_next(&mut layout)
        .push_next(&mut handle);
    let mut external = vk::ExternalImageFormatProperties::default();
    let mut properties = vk::ImageFormatProperties2::default().push_next(&mut external);
    unsafe {
        instance.get_physical_device_image_format_properties2(physical, &info, &mut properties)
    }
    .ok()?;
    let extent = properties.image_format_properties.max_extent;
    external
        .external_memory_properties
        .external_memory_features
        .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
        .then_some(extent)
}

pub(crate) fn import(device: Arc<wgpu::Device>, dmabuf: &Dmabuf<'_>) -> Result<ImportedBuffer> {
    let available = std::fs::File::from(dmabuf.fd.try_clone_to_owned()?)
        .seek(SeekFrom::End(0))
        .context("dma-buf size")?;
    ensure!(
        dmabuf.width > 0 && dmabuf.height > 0 && dmabuf.stride > 0,
        "invalid DMA-BUF dimensions"
    );
    let size = wgpu::Extent3d {
        width: dmabuf.width,
        height: dmabuf.height,
        depth_or_array_layers: 1,
    };
    let (image, memory, raw, family) = {
        let hal = unsafe { device.as_hal::<Vulkan>() }
            .ok_or_else(|| anyhow!("dma-buf import needs the Vulkan backend"))?;
        // Creating an image in a layout or size the device does not support
        // is undefined behavior, not an error.
        let max = layouts(&hal)
            .into_iter()
            .find_map(|(modifier, max)| (modifier == dmabuf.modifier).then_some(max))
            .ok_or_else(|| {
                anyhow!(
                    "the GPU cannot import dma-bufs with modifier {:#018x}",
                    dmabuf.modifier
                )
            })?;
        ensure!(
            (1..=max.width).contains(&dmabuf.width) && (1..=max.height).contains(&dmabuf.height),
            "the GPU imports at most {}x{} dma-bufs, not {}x{}",
            max.width,
            max.height,
            dmabuf.width,
            dmabuf.height
        );
        let raw = hal.raw_device();
        let external_fd =
            ash::khr::external_memory_fd::Device::new(hal.shared_instance().raw_instance(), raw);
        let plane = [vk::SubresourceLayout {
            offset: dmabuf.offset.into(),
            size: 0,
            row_pitch: dmabuf.stride.into(),
            array_pitch: 0,
            depth_pitch: 0,
        }];
        let mut layout = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(dmabuf.modifier)
            .plane_layouts(&plane);
        let mut external = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(FORMAT)
            .extent(vk::Extent3D {
                width: dmabuf.width,
                height: dmabuf.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut layout)
            .push_next(&mut external);
        let image = unsafe { raw.create_image(&info, None) }.with_context(|| {
            format!(
                "no {}x{} image with modifier {:#018x}, stride {}",
                dmabuf.width, dmabuf.height, dmabuf.modifier, dmabuf.stride
            )
        })?;
        let memory = (|| -> Result<vk::DeviceMemory> {
            let requirements = unsafe { raw.get_image_memory_requirements(image) };
            ensure!(
                available >= requirements.size,
                "dma-buf holds {available} bytes, the image needs {}",
                requirements.size
            );
            let mut fd_properties = vk::MemoryFdPropertiesKHR::default();
            unsafe {
                external_fd.get_memory_fd_properties(
                    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                    dmabuf.fd.as_raw_fd(),
                    &mut fd_properties,
                )
            }?;
            let memory_types = requirements.memory_type_bits & fd_properties.memory_type_bits;
            ensure!(memory_types != 0, "no memory type can hold this dma-buf");
            // A successful import takes ownership of the duplicate.
            let duplicate = dmabuf.fd.try_clone_to_owned()?;
            let mut import = vk::ImportMemoryFdInfoKHR::default()
                .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .fd(duplicate.as_raw_fd());
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let allocate = vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_types.trailing_zeros())
                .push_next(&mut import)
                .push_next(&mut dedicated);
            let memory = unsafe { raw.allocate_memory(&allocate, None) }?;
            let _ = duplicate.into_raw_fd();
            if let Err(error) = unsafe { raw.bind_image_memory(image, memory, 0) } {
                unsafe { raw.free_memory(memory, None) };
                return Err(error.into());
            }
            Ok(memory)
        })();
        match memory {
            Ok(memory) => (image, memory, raw.clone(), hal.queue_family_index()),
            Err(error) => {
                unsafe { raw.destroy_image(image, None) };
                return Err(error);
            }
        }
    };
    Ok(ImportedBuffer {
        _device: device,
        raw,
        image,
        memory,
        family,
        size,
    })
}

pub(crate) struct ImportedBuffer {
    _device: Arc<wgpu::Device>,
    raw: ash::Device,
    image: vk::Image,
    memory: vk::DeviceMemory,
    family: u32,
    pub(crate) size: wgpu::Extent3d,
}

impl ImportedBuffer {
    pub(crate) fn copy_to(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Texture,
        origin: Point<DevicePixels>,
        bounds: Bounds<DevicePixels>,
    ) -> Result<()> {
        encoder.transition_resources(
            std::iter::empty(),
            std::iter::once(wgpu::TextureTransition {
                texture: target,
                selector: None,
                state: wgpu::TextureUses::COPY_DST,
            }),
        );
        // SAFETY: target belongs to this device and stays live through submission.
        let target =
            unsafe { target.as_hal::<Vulkan>() }.context("DMA-BUF destination is not Vulkan")?;
        // SAFETY: only records into wgpu's open command buffer. The source is
        // retained until GPU completion; WebKit finished rendering before delivery.
        unsafe {
            let target = target.raw_handle();
            encoder.as_hal_mut::<Vulkan, _, _>(|encoder| -> Result<()> {
                let encoder = encoder.context("DMA-BUF command encoder is not Vulkan")?;
                let command = encoder.raw_handle();
                let subresources = vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1);
                let acquire = vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                    .dst_queue_family_index(self.family)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .image(self.image)
                    .subresource_range(subresources);
                self.raw.cmd_pipeline_barrier(
                    command,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[acquire],
                );
                let layers = vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .layer_count(1);
                let copy = vk::ImageCopy::default()
                    .src_subresource(layers)
                    .dst_subresource(layers)
                    .src_offset(vk::Offset3D {
                        x: origin.x.0,
                        y: origin.y.0,
                        z: 0,
                    })
                    .dst_offset(vk::Offset3D {
                        x: bounds.origin.x.0,
                        y: bounds.origin.y.0,
                        z: 0,
                    })
                    .extent(vk::Extent3D {
                        width: bounds.size.width.0 as u32,
                        height: bounds.size.height.0 as u32,
                        depth: 1,
                    });
                self.raw.cmd_copy_image(
                    command,
                    self.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    target,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[copy],
                );
                let release = vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(self.family)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                    .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .image(self.image)
                    .subresource_range(subresources);
                self.raw.cmd_pipeline_barrier(
                    command,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[release],
                );
                Ok(())
            })?;
        }
        Ok(())
    }
}

impl Drop for ImportedBuffer {
    fn drop(&mut self) {
        // SAFETY: the atlas keeps submitted reads alive until GPU completion.
        // `device` keeps these Vulkan handles valid through destruction.
        unsafe {
            self.raw.destroy_image(self.image, None);
            self.raw.free_memory(self.memory, None);
        }
    }
}

pub(crate) trait DmabufAdapter {
    fn request_dmabuf_device(
        &self,
        desc: &wgpu::DeviceDescriptor<'_>,
    ) -> Option<(wgpu::Device, wgpu::Queue)>;
}
impl DmabufAdapter for wgpu::Adapter {
    fn request_dmabuf_device(
        &self,
        desc: &wgpu::DeviceDescriptor<'_>,
    ) -> Option<(wgpu::Device, wgpu::Queue)> {
        // wgpu checks these when it opens the device itself, not from here: let
        // that path report what the adapter lacks.
        if !self.features().contains(desc.required_features)
            || !desc.required_limits.check_limits(&self.limits())
        {
            return None;
        }
        let open = {
            let hal = unsafe { self.as_hal::<Vulkan>() }?;
            let capabilities = hal.physical_device_capabilities();
            let version = capabilities
                .properties()
                .api_version
                .min(hal.shared_instance().instance_api_version());
            // Vulkan 1.2 holds every extension this one depends on.
            if version < vk::API_VERSION_1_2
                || !IMPORT_EXTENSIONS
                    .iter()
                    .all(|name| capabilities.supports_extension(name))
            {
                return None;
            }
            let callback: Box<wgpu::hal::vulkan::CreateDeviceCallback> = Box::new(|args| {
                for name in IMPORT_EXTENSIONS {
                    if !args.extensions.contains(&name) {
                        args.extensions.push(name);
                    }
                }
            });
            let open = unsafe {
                hal.open_with_callback(
                    desc.required_features,
                    &desc.required_limits,
                    &desc.memory_hints,
                    Some(callback),
                )
            };
            match open {
                Ok(open) => open,
                Err(error) => {
                    log::warn!("no device that imports tiled dma-bufs: {error}");
                    return None;
                }
            }
        };
        unsafe { self.create_device_from_hal(open, desc) }
            .inspect_err(|error| log::warn!("no device that imports tiled dma-bufs: {error}"))
            .ok()
    }
}
pub(crate) trait DmabufDevice {
    fn render_node(&self) -> Option<PathBuf>;
    fn importable_modifiers(&self) -> Vec<u64>;
}
impl DmabufDevice for wgpu::Device {
    fn render_node(&self) -> Option<PathBuf> {
        let hal = unsafe { self.as_hal::<Vulkan>() }?;
        let instance = hal.shared_instance();
        if instance.instance_api_version() < vk::API_VERSION_1_1 {
            return None;
        }
        let raw = instance.raw_instance();
        let physical = hal.raw_physical_device();
        let supported = unsafe { raw.enumerate_device_extension_properties(physical) }
            .ok()?
            .iter()
            .any(|extension| {
                extension.extension_name_as_c_str() == Ok(ash::ext::physical_device_drm::NAME)
            });
        if !supported {
            return None;
        }
        let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
        let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
        unsafe { raw.get_physical_device_properties2(physical, &mut properties) };
        if drm.has_render == vk::FALSE {
            return None;
        }
        device_node(drm.render_major as u64, drm.render_minor as u64)
    }

    fn importable_modifiers(&self) -> Vec<u64> {
        unsafe { self.as_hal::<Vulkan>() }
            .map(|hal| {
                layouts(&hal)
                    .into_iter()
                    .map(|(modifier, _)| modifier)
                    .collect()
            })
            .unwrap_or_default()
    }
}
