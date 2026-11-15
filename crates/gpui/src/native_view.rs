//! Native views retained by a painted scene.

use crate::{
    Bounds, ContentMask, DrawOrder, FocusId, Hitbox, HitboxId, Pixels, ScaledPixels, Size, WindowId,
};
use anyhow::{Result, ensure};
use raw_window_handle::HasWindowHandle;
use std::{cell::Cell, ops::Deref, rc::Rc};

/// Native composition capabilities supplied by a window backend.
#[derive(Clone, Copy, Debug, Default)]
#[expect(missing_docs)]
pub struct NativeCapabilities {
    pub views: bool,
    pub materials: bool,
}

/// A finite, positive page viewport in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport(Size<Pixels>);

impl TryFrom<Size<Pixels>> for Viewport {
    type Error = anyhow::Error;

    fn try_from(size: Size<Pixels>) -> Result<Self> {
        ensure!(
            size.width.0.is_finite()
                && size.height.0.is_finite()
                && size.width.0 > 0.
                && size.height.0 > 0.,
            "invalid native view viewport"
        );
        Ok(Self(size))
    }
}

impl Deref for Viewport {
    type Target = Size<Pixels>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
#[expect(missing_docs)]
pub struct FocusRequest {
    pub generation: u64,
    pub active: bool,
    pub window: Option<WindowId>,
}

/// One native view's input ownership, shared across cached scenes.
#[derive(Default)]
#[expect(missing_docs)]
pub struct NativeViewState {
    pub enabled: Cell<bool>,
    pub focus: Cell<FocusRequest>,
    pub window: Cell<Option<WindowId>>,
}

impl NativeViewState {
    /// Issue a fresh keyboard-focus request for the compositor to consume once.
    pub fn request_focus(&self, active: bool, window: Option<WindowId>) {
        self.focus.set(FocusRequest {
            generation: self.focus.get().generation.wrapping_add(1),
            active,
            window,
        });
    }
}

/// A retained native view at one position in the scene. The handle must name
/// the embedded view, not the containing GPUI window. The compositor owns its
/// superview, visibility and frame while it is hosted.
#[derive(Clone)]
#[expect(missing_docs)]
pub struct NativeView {
    pub view: Rc<dyn HasWindowHandle>,
    pub viewport: Viewport,
    pub state: Rc<NativeViewState>,
    pub focus: Option<FocusId>,
    pub focused: bool,
    pub window: Option<WindowId>,
    pub presented: Option<Rc<dyn Fn()>>,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub order: DrawOrder,
    pub parent: Option<usize>,
    pub opacity: f32,
    pub interactive: bool,
    pub hitbox: Option<HitboxId>,
    pub occlusions: Vec<Bounds<Pixels>>,
}

impl NativeView {
    /// Retain content; `Window::paint_native_view` assigns its scene geometry.
    pub fn new(
        view: Rc<dyn HasWindowHandle>,
        viewport: Viewport,
        state: Rc<NativeViewState>,
    ) -> Self {
        Self {
            view,
            viewport,
            state,
            focus: None,
            focused: false,
            window: None,
            presented: None,
            bounds: Bounds::default(),
            content_mask: ContentMask::default(),
            order: 0,
            parent: None,
            opacity: 1.,
            interactive: true,
            hitbox: None,
            occlusions: Vec::new(),
        }
    }

    pub(crate) fn update_occlusions(&mut self, hitboxes: &[Hitbox]) {
        self.occlusions.clear();
        let Some(index) = self
            .hitbox
            .and_then(|id| hitboxes.iter().position(|hitbox| hitbox.id == id))
        else {
            self.interactive = false;
            return;
        };
        self.occlusions.extend(
            hitboxes[index + 1..]
                .iter()
                .map(|hitbox| hitbox.bounds.intersect(&hitbox.content_mask.bounds))
                .filter(|bounds| !bounds.is_empty()),
        );
    }
}
