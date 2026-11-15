use crate::window_appearance;
use anyhow::{Context as _, Result};
use gpui::{self, Bounds, Pixels, native_material::MaterialState, native_view::NativeViewState};
use objc2::{
    DeclaredClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
};
use objc2_app_kit::{NSAppearanceCustomization, NSView};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{CGMutablePath, CGPath};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect};
use objc2_quartz_core::CAShapeLayer;
use std::{cell::RefCell, ptr, rc::Rc};

#[derive(Default)]
pub(crate) struct Region {
    pub(crate) interactive: bool,
    pub(crate) input: Option<Rc<NativeViewState>>,
    pub(crate) occlusions: Vec<Bounds<Pixels>>,
    pub(crate) path: Option<CFRetained<CGPath>>,
    pub(crate) material: Option<(MaterialState, Rc<dyn Fn()>)>,
}

impl Region {
    fn contains(&self, position: NSPoint) -> bool {
        self.interactive
            && self.input.as_ref().is_none_or(|state| state.enabled.get())
            && !self.occlusions.iter().any(|bounds| {
                bounds.contains(&gpui::point(
                    gpui::px(position.x as f32),
                    gpui::px(position.y as f32),
                ))
            })
            && self.path.as_ref().is_none_or(|path| {
                // SAFETY: null requests no additional transform.
                unsafe {
                    CGPath::contains_point(
                        Some(path),
                        ptr::null(),
                        CGPoint::new(position.x, position.y),
                        false,
                    )
                }
            })
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GPUINativeCompositionView"]
    #[ivars = RefCell<Region>]
    pub(crate) struct CompositionView;

    unsafe impl NSObjectProtocol for CompositionView {}

    impl CompositionView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool { true }

        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn appearance_changed(&self) {
            // SAFETY: NSView implements this main-thread notification.
            unsafe { let _: () = msg_send![super(self), viewDidChangeEffectiveAppearance]; }
            self.report_appearance();
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, position: NSPoint) -> Option<Retained<NSView>> {
            // SAFETY: AppKit invokes hitTest on the main thread with a point in the superview.
            let local = self.convertPoint_fromView(position, unsafe { self.superview() }.as_deref());
            if self.ivars().borrow().contains(local) {
                // SAFETY: invokes NSView's hit testing with its documented parent coordinates.
                let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: position] };
                hit.filter(|view| !ptr::eq(&**view, &**self))
            } else {
                None
            }
        }
    }
);

impl CompositionView {
    pub(crate) fn report_appearance(&self) {
        let hook = self.ivars().borrow().material.clone();
        if let Some((state, changed)) = hook {
            let appearance = self.effectiveAppearance();
            let appearance = window_appearance::window_appearance_from_native(Some(&appearance));
            if state.set_appearance(appearance) {
                changed();
            }
        }
    }

    pub(crate) fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let view = Self::alloc(mtm).set_ivars(RefCell::new(Region::default()));
        // SAFETY: initializes a newly allocated NSView subclass.
        let view: Retained<Self> = unsafe { msg_send![super(view), initWithFrame: NSRect::ZERO] };
        view.setWantsLayer(true);
        view.setAutoresizesSubviews(false);
        view.setClipsToBounds(true);
        view
    }
}

impl CompositionView {
    pub(crate) fn set_mask(
        &self,
        mask: gpui::ContentMask<gpui::ScaledPixels>,
        origin: gpui::Point<Pixels>,
        scale: f32,
    ) -> Result<()> {
        let logical = mask.bounds.map(|value| gpui::px(value.0 / scale));
        let path = CGMutablePath::new();
        let left = f64::from(logical.left() - origin.x);
        let top = f64::from(logical.top() - origin.y);
        let right = f64::from(logical.right() - origin.x);
        let bottom = f64::from(logical.bottom() - origin.y);
        let limit = ((right - left).min(bottom - top) * 0.5).max(0.);
        let corners = mask
            .corner_radii
            .map(|radius| (f64::from(*radius) / scale as f64).clamp(0., limit));
        // SAFETY: path is live and all transforms are null (identity).
        unsafe {
            CGMutablePath::move_to_point(Some(&path), ptr::null(), left + corners.top_left, top);
            CGMutablePath::add_arc_to_point(
                Some(&path),
                ptr::null(),
                right,
                top,
                right,
                bottom,
                corners.top_right,
            );
            CGMutablePath::add_arc_to_point(
                Some(&path),
                ptr::null(),
                right,
                bottom,
                left,
                bottom,
                corners.bottom_right,
            );
            CGMutablePath::add_arc_to_point(
                Some(&path),
                ptr::null(),
                left,
                bottom,
                left,
                top,
                corners.bottom_left,
            );
            CGMutablePath::add_arc_to_point(
                Some(&path),
                ptr::null(),
                left,
                top,
                right,
                top,
                corners.top_left,
            );
        }
        CGMutablePath::close_subpath(Some(&path));
        let shape = CAShapeLayer::new();
        shape.setPath(Some(&path));
        if let Some(layer) = self.layer() {
            // SAFETY: the mask is a new layer, not an ancestor of the clipped layer.
            unsafe {
                layer.setMask(Some(&shape));
            }
        }
        self.ivars().borrow_mut().path =
            Some(CGPath::new_copy(Some(&path)).context("failed to copy native clip")?);
        Ok(())
    }
}
