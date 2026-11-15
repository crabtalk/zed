use crate::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, Bounds, Context, DevicePixels, Pixels, Render,
    Size, Task, Window, WindowBounds, WindowOptions, point, px,
};
use anyhow::{Result, anyhow};
use image::RgbaImage;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(16);
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_DIMENSION: f32 = 16_384.;

#[derive(Default)]
pub(crate) struct Capture {
    pub pending: bool,
    pub error: Option<String>,
}

struct Closing {
    window: AnyWindowHandle,
    cx: AsyncApp,
}

impl Drop for Closing {
    fn drop(&mut self) {
        let window = self.window;
        self.cx
            .spawn(async move |cx| {
                let _ = cx.update_window(window, |_, window, _| window.remove_window());
            })
            .detach();
    }
}

impl App {
    /// Renders a view in an isolated hidden window. `scale` is output pixels per
    /// logical pixel. Image loads must complete within 30 seconds.
    pub fn render_view_to_image<V: Render + 'static>(
        &mut self,
        size: Size<Pixels>,
        scale: f32,
        build: impl FnOnce(&mut Window, &mut Context<V>) -> V + 'static,
    ) -> Result<Task<Result<RgbaImage>>> {
        let physical = size.map(|length| f32::from(length) * scale);
        anyhow::ensure!(
            scale.is_finite() && scale > 0.,
            "image scale must be finite and positive"
        );
        anyhow::ensure!(
            physical.width.is_finite()
                && physical.height.is_finite()
                && physical.width > 0.
                && physical.height > 0.
                && physical.width <= MAX_DIMENSION
                && physical.height <= MAX_DIMENSION,
            "image dimensions must be between 1 and 16384 pixels"
        );
        let handle = self.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size,
                ))),
                focus: false,
                show: false,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| build(window, cx)),
        )?;
        let handle = handle.into();
        let closing = Closing {
            window: handle,
            cx: self.to_async(),
        };
        Ok(self.spawn(async move |cx| {
            let _closing = closing;
            let started = Instant::now();
            loop {
                let image = cx.update_window(handle, |_, window, cx| {
                    window.viewport_size = size;
                    window.scale_factor = scale;
                    window.capture = Some(Capture::default());
                    window.refresh();
                    window.draw(cx).clear(cx);
                    let capture = window.capture.take().expect("capture was installed");
                    if let Some(error) = capture.error {
                        return Err(anyhow!("image failed to load: {error}"));
                    }
                    if capture.pending {
                        return Ok(None);
                    }
                    let pixels = physical.map(|length| DevicePixels(length.ceil() as i32));
                    window
                        .platform_window
                        .render_scene_to_image(&window.rendered_frame.scene, pixels)
                        .map(Some)
                })??;
                if let Some(image) = image {
                    return Ok(image);
                }
                anyhow::ensure!(
                    started.elapsed() < TIMEOUT,
                    "image loading timed out during rendering"
                );
                cx.background_executor().timer(POLL).await;
            }
        }))
    }
}
