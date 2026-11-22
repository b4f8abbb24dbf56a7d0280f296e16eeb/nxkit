//! The swapchain: its images, acquire and present.

use std::collections::HashMap;

use crate::backend::Present;
use crate::desc::{ImageDesc, TextureId, TextureKind};
use crate::format;
use crate::info::SwapchainInfo;
use crate::result::{Status, VkError, VkResult};

use super::Vk;
use super::state::{Image, Signal, Swapchain};

impl Vk {
    /// `vkCreateSwapchainKHR`: 2 to 4 images of the format, made by the backend as textures of their own.
    pub fn create_swapchain(&self, info: &SwapchainInfo) -> VkResult<u64> {
        let Some(f) = format::texture_format(info.format) else {
            let e = VkError::FormatNotSupported;
            return Err(self.log.unsupported(
                "swapchain format",
                e,
                format_args!("swap chain format {}", info.format),
            ));
        };
        let n = info.min_images.clamp(2, 4);
        let desc = ImageDesc {
            image_type: crate::consts::image_type::D2,
            format: info.format,
            pixel: f.pixel.unwrap_or(format::PixelFormat::Bgra8Unorm),
            kind: TextureKind::D2,
            extent: [info.extent.0.max(1), info.extent.1.max(1), 1],
            levels: 1,
            layers: 1,
            samples: 1,
            usage: info.usage,
            flags: 0,
            format_views: false,
            linear: None,
            conversion: format::Conversion::None,
            swapchain: true,
        };
        let size = f.image_bytes(desc.extent[0], desc.extent[1], 1, 1, 1, 1);
        let mut images = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let h = self.lock().images.insert(Image {
                desc: desc.clone(),
                pitch: 0,
                size,
                created: true,
            });
            if let Err(e) = self.backend.create_image(TextureId(h), &desc) {
                self.log.once(
                    "swapchain image",
                    format_args!("[RED] cannot make a swapchain image: {e}"),
                );
            }
            images.push(h);
        }
        eprintln!(
            "[vk] swap chain {}x{} format {}, {n} images (at least {} requested), present mode {}",
            info.extent.0, info.extent.1, info.format, info.min_images, info.present_mode
        );
        Ok(self.lock().swapchains.insert(Swapchain {
            images,
            extent: info.extent,
            format: info.format,
            next: 0,
        }))
    }

    /// `vkDestroySwapchainKHR`: its images go with it.
    pub fn destroy_swapchain(&self, swapchain: u64) {
        let images = {
            let mut s = self.lock();
            let Some(sc) = s.swapchains.remove(swapchain) else {
                return;
            };
            for &i in &sc.images {
                s.images.remove(i);
            }
            sc.images
        };
        for i in images {
            self.backend.destroy_texture(TextureId(i));
        }
    }

    /// `vkGetSwapchainImagesKHR`.
    pub fn swapchain_images(&self, swapchain: u64) -> VkResult<Vec<u64>> {
        let s = self.lock();
        Ok(s.swapchains
            .get(swapchain)
            .ok_or_else(|| self.bad("swapchain", swapchain))?
            .images
            .clone())
    }

    /// `vkAcquireNextImageKHR`: the images in turn. The backend paces and waits until the image's previous present has
    /// been shown. The fence (0 for none) is signalled at once, the semaphore needs nothing.
    pub fn acquire_next_image(
        &self,
        swapchain: u64,
        timeout_ns: u64,
        fence: u64,
    ) -> VkResult<(Status, u32)> {
        let (index, image) = {
            let mut s = self.lock();
            let sc = s
                .swapchains
                .get_mut(swapchain)
                .ok_or_else(|| self.bad("swapchain", swapchain))?;
            let n = sc.images.len().max(1) as u32;
            let i = sc.next % n;
            sc.next = sc.next.wrapping_add(1);
            (i, sc.images.get(i as usize).copied().unwrap_or(0))
        };
        if !self.backend.acquire(TextureId(image), timeout_ns) {
            return Ok((
                if timeout_ns == 0 {
                    Status::NotReady
                } else {
                    Status::Timeout
                },
                index,
            ));
        }
        if let Some(f) = self.lock().fences.get_mut(fence) {
            *f = Signal::Signaled;
        }
        Ok((Status::Success, index))
    }

    /// `vkQueuePresentKHR`: each swapchain image is shown once the last submit has completed. One result per image.
    pub fn present(&self, images: &[(u64, u32)]) -> Vec<VkResult<()>> {
        let after = self.last_serial();
        let presents: Vec<VkResult<Present>> = {
            let s = self.lock();
            images
                .iter()
                .map(|&(sc, index)| {
                    let chain = s
                        .swapchains
                        .get(sc)
                        .ok_or_else(|| self.bad("swapchain", sc))?;
                    let image = *chain.images.get(index as usize).ok_or(VkError::OutOfDate)?;
                    Ok(Present {
                        swapchain: sc,
                        index,
                        image: TextureId(image),
                        extent: chain.extent,
                        format: chain.format,
                        after,
                    })
                })
                .collect()
        };
        presents
            .into_iter()
            .map(|p| {
                let p = p?;
                self.backend.present(&p).map_err(|e| {
                    self.log.once("present", format_args!("[RED] present: {e}"));
                    VkError::SurfaceLost
                })
            })
            .collect()
    }

    /// The live objects by kind, for diagnostics.
    pub fn live_objects(&self) -> HashMap<&'static str, usize> {
        let s = self.lock();
        HashMap::from([
            ("memory", s.memories.len()),
            ("buffer", s.buffers.len()),
            ("image", s.images.len()),
            ("image view", s.views.len()),
            ("sampler", s.samplers.len()),
            ("descriptor set", s.sets.len()),
            ("pipeline", s.pipelines.len()),
            ("command buffer", s.command_buffers.len()),
            ("fence", s.fences.len()),
        ])
    }
}
