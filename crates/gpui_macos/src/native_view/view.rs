use crate::native_view::{self, clip::CompositionView};
use anyhow::{Context as _, Result};
use gpui::{
    self, Bounds, Pixels, Point,
    native_view::{FocusRequest, NativeView, NativeViewState},
};
use objc2::{DeclaredClass, MainThreadMarker, Message, rc::Retained};
use objc2_app_kit::NSView;
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::RawWindowHandle;
use std::{ptr, rc::Rc};

pub(crate) struct HostedView {
    pub(crate) source: NativeView,
    pub(crate) native: Retained<NSView>,
    pub(crate) clip: Retained<CompositionView>,
    content: Retained<CompositionView>,
    focus: Option<FocusRequest>,
}

impl HostedView {
    pub(crate) fn new(source: &NativeView, mtm: MainThreadMarker) -> Result<Self> {
        let RawWindowHandle::AppKit(handle) = source
            .view
            .window_handle()
            .map_err(|error| anyhow::anyhow!("native view handle: {error:?}"))?
            .as_raw()
        else {
            anyhow::bail!("native composition requires an AppKit view handle");
        };
        // SAFETY: HasWindowHandle keeps the NSView alive; retain it while hosted.
        let native = unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }
            .context("missing native view")?;
        let clip = CompositionView::new(mtm);
        let content = CompositionView::new(mtm);
        content.ivars().borrow_mut().interactive = true;
        clip.addSubview(&content);
        content.addSubview(&native);
        Ok(Self {
            source: source.clone(),
            native,
            clip,
            content,
            focus: None,
        })
    }

    pub(crate) fn update(
        &mut self,
        source: &NativeView,
        scale: f32,
        parent_origin: Point<Pixels>,
    ) -> Result<()> {
        if !self.owns_native() {
            self.content.addSubview(&self.native);
            self.focus = None;
        }
        if !Rc::ptr_eq(&self.source.state, &source.state) {
            self.focus = None;
        }
        let bounds = source.bounds.map(|value| gpui::px(value.0 / scale));
        let mask = source
            .content_mask
            .bounds
            .map(|value| gpui::px(value.0 / scale));
        let visible = bounds.intersect(&mask);
        self.clip.setHidden(source.opacity <= 0.);
        self.clip.setFrame(native_view::rect(Bounds::new(
            visible.origin - parent_origin,
            visible.size,
        )));
        self.clip.setAlphaValue(source.opacity as f64);
        self.clip.setBoundsSize(NSSize::new(
            f64::from(visible.size.width),
            f64::from(visible.size.height),
        ));
        self.content.setFrame(native_view::rect(Bounds::new(
            bounds.origin - visible.origin,
            bounds.size,
        )));
        self.content.setBoundsSize(NSSize::new(
            f64::from(source.viewport.width),
            f64::from(source.viewport.height),
        ));
        let viewport = NSRect::new(
            NSPoint::ZERO,
            NSSize::new(
                f64::from(source.viewport.width),
                f64::from(source.viewport.height),
            ),
        );
        if self.native.frame() != viewport {
            self.native.setFrame(viewport);
        }
        self.native.setHidden(false);
        self.clip
            .set_mask(source.content_mask, visible.origin, scale)?;
        let mut region = self.clip.ivars().borrow_mut();
        region.interactive = source.interactive && source.opacity > 0.;
        region.input = Some(source.state.clone());
        region.occlusions = source
            .occlusions
            .iter()
            .map(|bounds| Bounds::new(bounds.origin - visible.origin, bounds.size))
            .collect();
        source.state.window.set(source.window);
        let request = source.state.focus.get();
        if source.focused && request.window != source.window {
            source.state.request_focus(true, source.window);
        }
        self.source = source.clone();
        Ok(())
    }
    pub(crate) fn owns_native(&self) -> bool {
        // SAFETY: both views belong to this main-thread host.
        unsafe { self.native.superview() }.is_some_and(|view| ptr::eq(&*view, &**self.content))
    }

    pub(crate) fn focus_change(&mut self, parent: &NSView) -> Option<FocusChange> {
        let request = self.source.state.focus.get();
        let previous = self.focus.replace(request);
        if !self.source.interactive || self.source.opacity <= 0. || !self.source.state.enabled.get()
        {
            return Some(FocusChange::Release {
                native: self.native.clone(),
                parent: parent.retain(),
            });
        }
        if previous == Some(request) {
            return None;
        }
        Some(
            if request.active
                && (request.window.is_none() || request.window == self.source.window)
                && (self.source.focus.is_none() || self.source.focused)
            {
                FocusChange::Acquire {
                    native: self.native.clone(),
                    state: self.source.state.clone(),
                    request,
                }
            } else {
                FocusChange::Release {
                    native: self.native.clone(),
                    parent: parent.retain(),
                }
            },
        )
    }
}

impl Drop for HostedView {
    fn drop(&mut self) {
        if self.owns_native() {
            self.source.state.window.set(None);
            self.native.setHidden(true);
            self.native.removeFromSuperview();
        }
        self.clip.removeFromSuperview();
    }
}

pub(crate) enum FocusChange {
    Release {
        native: Retained<NSView>,
        parent: Retained<NSView>,
    },
    Acquire {
        native: Retained<NSView>,
        state: Rc<NativeViewState>,
        request: FocusRequest,
    },
}

impl FocusChange {
    pub(crate) fn apply(self) {
        let (native, target, acquiring) = match self {
            Self::Release { native, parent } => (native, parent, false),
            Self::Acquire {
                native,
                state,
                request,
            } => {
                if !state.enabled.get() || state.focus.get() != request {
                    return;
                }
                (native.clone(), native, true)
            }
        };
        let Some(window) = target.window() else {
            return;
        };
        let owns_focus = window.firstResponder().is_some_and(|responder| {
            responder
                .downcast_ref::<NSView>()
                .is_some_and(|view| ptr::eq(view, &*native) || view.isDescendantOf(&native))
        });
        if owns_focus != acquiring {
            window.makeFirstResponder(Some(&target));
        }
    }
}
