//! Native material scopes in a GPU scene.

use crate::{
    Bounds, ContentMask, DrawOrder, GlobalElementId, Hsla, Pixels, ScaledPixels, WindowAppearance,
};
use std::{cell::Cell, rc::Rc};

/// The platform owns optical treatment and accessibility adjustments.
#[derive(Clone, Copy, Debug, PartialEq)]
#[expect(missing_docs)]
pub enum MaterialStyle {
    Popover {
        corner_radius: Pixels,
    },
    Glass {
        clear: bool,
        tint: Option<Hsla>,
        corner_radius: Pixels,
        interactive: bool,
    },
    Group {
        spacing: Pixels,
    },
}

/// Retained per element; appearance applies only to this scope's foreground.
#[derive(Clone, Default)]
pub struct MaterialState {
    pub(crate) appearance: Rc<Cell<Option<WindowAppearance>>>,
}

impl MaterialState {
    /// The effective content appearance last reported by the native host.
    pub fn appearance(&self) -> Option<WindowAppearance> {
        self.appearance.get()
    }

    /// Updates the appearance reported by a platform host.
    pub fn set_appearance(&self, appearance: WindowAppearance) -> bool {
        self.appearance.replace(Some(appearance)) != Some(appearance)
    }
}

/// Geometry uses scene coordinates. Children occupy `order..end_order`.
#[derive(Clone)]
#[expect(missing_docs)]
pub struct NativeMaterial {
    pub id: GlobalElementId,
    pub state: MaterialState,
    pub style: MaterialStyle,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub opacity: f32,
    pub order: DrawOrder,
    pub end_order: DrawOrder,
    pub parent: Option<usize>,
}
