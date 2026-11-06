use ash::vk;
use std::mem;

impl super::Surface {
    pub fn info(&self) -> crate::SurfaceInfo {
        crate::SurfaceInfo {
            format: self.swapchain.format,
            alpha: self.swapchain.alpha,
        }
    }

    unsafe fn deinit_swapchain(&mut self, raw_device: &ash::Device) {
        let _ = raw_device.device_wait_idle();
        self.device
            .destroy_swapchain(mem::take(&mut self.swapchain.raw), None);
        for frame in self.frames.drain(..) {
            raw_device.destroy_image_view(frame.view, None);
            raw_device.destroy_semaphore(frame.acquire_semaphore, None);
            raw_device.destroy_semaphore(frame.present_semaphore, None);
        }
    }

    /// Creates a swapchain from `params`, retiring the current one.
    unsafe fn build_swapchain(&mut self, params: super::SwapchainParams) {
        let queue_families = [params.queue_family_index];
        let mut full_screen_exclusive_info = vk::SurfaceFullScreenExclusiveInfoEXT {
            full_screen_exclusive: params
                .full_screen_exclusive
                .unwrap_or(vk::FullScreenExclusiveEXT::DISALLOWED),
            ..Default::default()
        };
        let mut create_info = vk::SwapchainCreateInfoKHR {
            surface: self.raw,
            min_image_count: params.min_image_count,
            image_format: params.surface_format.format,
            image_color_space: params.surface_format.color_space,
            image_extent: params.extent,
            image_array_layers: 1,
            image_usage: params.usage,
            pre_transform: vk::SurfaceTransformFlagsKHR::IDENTITY,
            composite_alpha: params.composite_alpha,
            present_mode: params.present_mode,
            old_swapchain: self.swapchain.raw,
            ..Default::default()
        }
        .queue_family_indices(&queue_families);
        if params.full_screen_exclusive.is_some() {
            create_info = create_info.push_next(&mut full_screen_exclusive_info);
        }
        let raw_swapchain = self.device.create_swapchain(&create_info, None).unwrap();

        let core = self.core.clone();
        self.deinit_swapchain(&core);

        let images = self.device.get_swapchain_images(raw_swapchain).unwrap();
        let subresource_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let semaphore_create_info = vk::SemaphoreCreateInfo::default();
        for image in images {
            let view_create_info = vk::ImageViewCreateInfo {
                image,
                view_type: vk::ImageViewType::TYPE_2D,
                format: params.surface_format.format,
                subresource_range,
                ..Default::default()
            };
            let view = core.create_image_view(&view_create_info, None).unwrap();
            let acquire_semaphore = core.create_semaphore(&semaphore_create_info, None).unwrap();
            let present_semaphore = core.create_semaphore(&semaphore_create_info, None).unwrap();
            self.frames.push(super::InternalFrame {
                acquire_semaphore,
                present_semaphore,
                image,
                view,
            });
        }
        self.swapchain = super::Swapchain {
            raw: raw_swapchain,
            format: params.format,
            alpha: params.alpha,
            target_size: [params.extent.width as u16, params.extent.height as u16],
        };
        self.params = Some(params);
    }

    /// PulseTerm patch: an out-of-date swapchain is rebuilt here, with the
    /// settings it was last built from, and the image acquired again.
    ///
    /// Upstream returned a frame that is never presented and left rebuilding to
    /// the caller, which gpui 0.2.2 only does when the window size changes. On
    /// Wayland the driver also reports out of date when the compositor sends new
    /// dmabuf feedback -- niri does as soon as a window is on screen, offering a
    /// scanout format -- so the window froze on its third frame until resized.
    pub fn acquire_frame(&mut self) -> super::Frame {
        let frame = self.try_acquire_frame();
        if frame.image_index.is_some() {
            return frame;
        }
        match self.params {
            Some(mut params) => {
                log::info!("Rebuilding the out-of-date swapchain");
                // X11 and others fix the extent to the window's; Wayland
                // leaves it to us (0xFFFFFFFF) and the old one stands.
                if let Ok(caps) = unsafe {
                    self.khr_surface
                        .get_physical_device_surface_capabilities(self.physical_device, self.raw)
                } {
                    if caps.current_extent.width != u32::MAX {
                        params.extent = caps.current_extent;
                    }
                }
                unsafe { self.build_swapchain(params) };
                self.try_acquire_frame()
            }
            None => frame,
        }
    }

    fn try_acquire_frame(&mut self) -> super::Frame {
        let acquire_semaphore = self.next_semaphore;
        match unsafe {
            self.device.acquire_next_image(
                self.swapchain.raw,
                !0,
                acquire_semaphore,
                vk::Fence::null(),
            )
        } {
            Ok((index, _suboptimal)) => {
                self.next_semaphore = mem::replace(
                    &mut self.frames[index as usize].acquire_semaphore,
                    acquire_semaphore,
                );
                super::Frame {
                    internal: self.frames[index as usize],
                    swapchain: self.swapchain,
                    image_index: Some(index),
                }
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                log::warn!("Acquire failed because the surface is out of date");
                super::Frame {
                    internal: self.frames[0],
                    swapchain: self.swapchain,
                    image_index: None,
                }
            }
            Err(other) => panic!("Aquire image error {}", other),
        }
    }
}

impl super::Context {
    pub fn create_surface<
        I: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle,
    >(
        &self,
        window: &I,
    ) -> Result<super::Surface, crate::NotSupportedError> {
        let khr_swapchain = self
            .device
            .swapchain
            .clone()
            .ok_or(crate::NotSupportedError::NoSupportedDeviceFound)?;

        let raw = unsafe {
            ash_window::create_surface(
                &self.entry,
                &self.instance.core,
                window.display_handle().unwrap().as_raw(),
                window.window_handle().unwrap().as_raw(),
                None,
            )
            .map_err(super::PlatformError::Init)?
        };

        let khr_surface = self
            .instance
            .surface
            .as_ref()
            .ok_or(crate::NotSupportedError::PlatformNotSupported)?;
        if unsafe {
            khr_surface.get_physical_device_surface_support(
                self.physical_device,
                self.queue_family_index,
                raw,
            ) != Ok(true)
        } {
            log::warn!("Rejected for not presenting to the window surface");
            return Err(crate::NotSupportedError::PlatformNotSupported);
        }

        let mut surface_info = vk::PhysicalDeviceSurfaceInfo2KHR {
            surface: raw,
            ..Default::default()
        };
        let mut fullscreen_exclusive_win32 = vk::SurfaceFullScreenExclusiveWin32InfoEXT::default();
        surface_info = surface_info.push_next(&mut fullscreen_exclusive_win32);
        let mut fullscreen_exclusive_ext = vk::SurfaceCapabilitiesFullScreenExclusiveEXT::default();
        let mut capabilities2_khr =
            vk::SurfaceCapabilities2KHR::default().push_next(&mut fullscreen_exclusive_ext);
        let _ = unsafe {
            self.instance
                .get_surface_capabilities2
                .as_ref()
                .unwrap()
                .get_physical_device_surface_capabilities2(
                    self.physical_device,
                    &surface_info,
                    &mut capabilities2_khr,
                )
        };
        log::debug!("{:?}", capabilities2_khr.surface_capabilities);

        let semaphore_create_info = vk::SemaphoreCreateInfo::default();
        let next_semaphore = unsafe {
            self.device
                .core
                .create_semaphore(&semaphore_create_info, None)
                .unwrap()
        };

        Ok(super::Surface {
            device: khr_swapchain,
            raw,
            frames: Vec::new(),
            next_semaphore,
            swapchain: super::Swapchain {
                raw: vk::SwapchainKHR::null(),
                format: crate::TextureFormat::Rgba8Unorm,
                alpha: crate::AlphaMode::Ignored,
                target_size: [0; 2],
            },
            full_screen_exclusive: fullscreen_exclusive_ext.full_screen_exclusive_supported != 0,
            core: self.device.core.clone(),
            khr_surface: khr_surface.clone(),
            physical_device: self.physical_device,
            params: None,
        })
    }

    pub fn destroy_surface(&self, surface: &mut super::Surface) {
        unsafe {
            surface.deinit_swapchain(&self.device.core);
            self.device
                .core
                .destroy_semaphore(surface.next_semaphore, None)
        };
        if let Some(ref surface_instance) = self.instance.surface {
            unsafe { surface_instance.destroy_surface(surface.raw, None) };
        }
    }

    pub fn reconfigure_surface(&self, surface: &mut super::Surface, config: crate::SurfaceConfig) {
        let khr_surface = self.instance.surface.as_ref().unwrap();

        let capabilities = unsafe {
            khr_surface
                .get_physical_device_surface_capabilities(self.physical_device, surface.raw)
                .unwrap()
        };
        if config.size.width < capabilities.min_image_extent.width
            || config.size.width > capabilities.max_image_extent.width
            || config.size.height < capabilities.min_image_extent.height
            || config.size.height > capabilities.max_image_extent.height
        {
            log::warn!(
                "Requested size {}x{} is outside of surface capabilities",
                config.size.width,
                config.size.height
            );
        }

        let (alpha, composite_alpha) = if config.transparent {
            if capabilities
                .supported_composite_alpha
                .contains(vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED)
            {
                (
                    crate::AlphaMode::PostMultiplied,
                    vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
                )
            } else if capabilities
                .supported_composite_alpha
                .contains(vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED)
            {
                (
                    crate::AlphaMode::PreMultiplied,
                    vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
                )
            } else {
                log::error!(
                    "No composite alpha flag for transparency: {:?}",
                    capabilities.supported_composite_alpha
                );
                (
                    crate::AlphaMode::Ignored,
                    vk::CompositeAlphaFlagsKHR::OPAQUE,
                )
            }
        } else {
            (
                crate::AlphaMode::Ignored,
                vk::CompositeAlphaFlagsKHR::OPAQUE,
            )
        };

        let (requested_frame_count, mode_preferences) = match config.display_sync {
            crate::DisplaySync::Block => (3, [vk::PresentModeKHR::FIFO].as_slice()),
            crate::DisplaySync::Recent => (
                3,
                [
                    vk::PresentModeKHR::MAILBOX,
                    vk::PresentModeKHR::FIFO_RELAXED,
                    vk::PresentModeKHR::IMMEDIATE,
                ]
                .as_slice(),
            ),
            crate::DisplaySync::Tear => (2, [vk::PresentModeKHR::IMMEDIATE].as_slice()),
        };
        let effective_frame_count = requested_frame_count.max(capabilities.min_image_count);

        let present_modes = unsafe {
            khr_surface
                .get_physical_device_surface_present_modes(self.physical_device, surface.raw)
                .unwrap()
        };
        let present_mode = *mode_preferences
            .iter()
            .find(|mode| present_modes.contains(mode))
            .unwrap();
        log::info!("Using surface present mode {:?}", present_mode);

        let queue_families = [self.queue_family_index];

        let mut supported_formats = Vec::new();
        let (format, surface_format) = if surface.swapchain.target_size[0] > 0 {
            let format = surface.swapchain.format;
            log::info!("Retaining current format: {:?}", format);
            let vk_color_space = match (format, config.color_space) {
                (crate::TextureFormat::Bgra8Unorm, crate::ColorSpace::Srgb) => {
                    vk::ColorSpaceKHR::SRGB_NONLINEAR
                }
                (crate::TextureFormat::Bgra8Unorm, crate::ColorSpace::Linear) => {
                    vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT
                }
                (crate::TextureFormat::Bgra8UnormSrgb, crate::ColorSpace::Linear) => {
                    vk::ColorSpaceKHR::default()
                }
                _ => panic!(
                    "Unexpected format {:?} under color space {:?}",
                    format, config.color_space
                ),
            };
            (
                format,
                vk::SurfaceFormatKHR {
                    format: super::map_texture_format(format),
                    color_space: vk_color_space,
                },
            )
        } else {
            supported_formats = unsafe {
                khr_surface
                    .get_physical_device_surface_formats(self.physical_device, surface.raw)
                    .unwrap()
            };
            match config.color_space {
                crate::ColorSpace::Linear => {
                    let surface_format = vk::SurfaceFormatKHR {
                        format: vk::Format::B8G8R8A8_UNORM,
                        color_space: vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT,
                    };
                    if supported_formats.contains(&surface_format) {
                        log::info!("Using linear SRGB color space");
                        (crate::TextureFormat::Bgra8Unorm, surface_format)
                    } else {
                        (
                            crate::TextureFormat::Bgra8UnormSrgb,
                            vk::SurfaceFormatKHR {
                                format: vk::Format::B8G8R8A8_SRGB,
                                color_space: vk::ColorSpaceKHR::default(),
                            },
                        )
                    }
                }
                crate::ColorSpace::Srgb => (
                    crate::TextureFormat::Bgra8Unorm,
                    vk::SurfaceFormatKHR {
                        format: vk::Format::B8G8R8A8_UNORM,
                        color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
                    },
                ),
            }
        };
        if !supported_formats.is_empty() && !supported_formats.contains(&surface_format) {
            log::error!("Surface formats are incompatible: {:?}", supported_formats);
        }

        let vk_usage = super::resource::map_texture_usage(config.usage, crate::TexelAspects::COLOR);
        if !capabilities.supported_usage_flags.contains(vk_usage) {
            log::error!(
                "Surface usages are incompatible: {:?}",
                capabilities.supported_usage_flags
            );
        }

        if surface.full_screen_exclusive {
            assert!(self.device.full_screen_exclusive.is_some());
            log::info!(
                "Configuring exclusive full screen: {}",
                config.allow_exclusive_full_screen
            );
        }
        let params = super::SwapchainParams {
            min_image_count: effective_frame_count,
            surface_format,
            extent: vk::Extent2D {
                width: config.size.width,
                height: config.size.height,
            },
            usage: vk_usage,
            composite_alpha,
            present_mode,
            queue_family_index: queue_families[0],
            format,
            alpha,
            full_screen_exclusive: surface.full_screen_exclusive.then_some(
                if config.allow_exclusive_full_screen {
                    vk::FullScreenExclusiveEXT::ALLOWED
                } else {
                    vk::FullScreenExclusiveEXT::DISALLOWED
                },
            ),
        };
        unsafe { surface.build_swapchain(params) };
    }
}
