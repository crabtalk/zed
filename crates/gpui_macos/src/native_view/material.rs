use crate::native_view::{self, clip::CompositionView};
use anyhow::Result;
use gpui::{
    self, Bounds, Pixels, Point, Rgba,
    native_material::{MaterialStyle, NativeMaterial},
};
use objc2::{
    DeclaredClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, rc::Retained,
    runtime::AnyClass, sel,
};
use objc2_app_kit::{
    NSColor, NSGlassEffectContainerView, NSGlassEffectView, NSGlassEffectViewStyle, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect};
use std::rc::Rc;

macro_rules! content_host {
    ($name:ident, $superclass:ident) => {
        define_class!(
            #[unsafe(super($superclass))]
            #[thread_kind = MainThreadOnly]
            #[ivars = Retained<CompositionView>]
            struct $name;
            unsafe impl NSObjectProtocol for $name {}
            impl $name {
                #[unsafe(method(isFlipped))]
                fn is_flipped(&self) -> bool { true }

                #[unsafe(method_id(hitTest:))]
                fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
                    let content = self.ivars();
                    // SAFETY: these retained views are used only on AppKit's main thread.
                    let content_parent = unsafe { content.superview() };
                    let source_parent = unsafe { self.superview() };
                    let position = content_parent.as_ref().map(|parent|
                        parent.convertPoint_fromView(point, source_parent.as_deref())
                    ).unwrap_or(point);
                    content.hitTest(position)
                }
            }
        );
        impl $name {
            fn new(content: &CompositionView, mtm: MainThreadMarker) -> Retained<Self> {
                let view = Self::alloc(mtm).set_ivars(content.retain());
                // SAFETY: initializes the newly allocated NSView subclass.
                unsafe { msg_send![super(view), initWithFrame: NSRect::ZERO] }
            }
        }
    };
}

content_host!(GlassHost, NSGlassEffectView);
content_host!(GroupHost, NSGlassEffectContainerView);
content_host!(LegacyHost, NSVisualEffectView);

enum Effect {
    Glass(Retained<GlassHost>),
    Group(Retained<GroupHost>),
    Legacy(Retained<LegacyHost>),
    Plain,
}

impl Effect {
    fn view(&self) -> Option<&NSView> {
        match self {
            Self::Glass(view) => Some(view),
            Self::Group(view) => Some(view),
            Self::Legacy(view) => Some(view),
            Self::Plain => None,
        }
    }
}

pub(crate) struct MaterialHost {
    pub(crate) source: NativeMaterial,
    pub(crate) clip: Retained<CompositionView>,
    pub(crate) content: Retained<CompositionView>,
    effect: Effect,
}

impl MaterialHost {
    pub(crate) fn new(
        source: &NativeMaterial,
        changed: Rc<dyn Fn()>,
        mtm: MainThreadMarker,
    ) -> Self {
        let clip = CompositionView::new(mtm);
        clip.ivars().borrow_mut().interactive = true;
        let content = CompositionView::new(mtm);
        content.ivars().borrow_mut().interactive = true;
        if matches!(source.style, MaterialStyle::Group { .. }) {
            content.setClipsToBounds(false);
        }
        content.ivars().borrow_mut().material = Some((source.state.clone(), changed));
        let effect = match source.style {
            MaterialStyle::Glass { .. } if AnyClass::get(c"NSGlassEffectView").is_some() => {
                let view = GlassHost::new(&content, mtm);
                view.setContentView(Some(&content));
                Effect::Glass(view)
            }
            MaterialStyle::Glass { .. } | MaterialStyle::Popover { .. } => {
                let view = LegacyHost::new(&content, mtm);
                view.setWantsLayer(true);
                view.setMaterial(NSVisualEffectMaterial::Popover);
                view.setBlendingMode(NSVisualEffectBlendingMode::WithinWindow);
                view.setState(NSVisualEffectState::FollowsWindowActiveState);
                view.addSubview(&content);
                Effect::Legacy(view)
            }
            MaterialStyle::Group { .. }
                if AnyClass::get(c"NSGlassEffectContainerView").is_some() =>
            {
                let view = GroupHost::new(&content, mtm);
                view.setContentView(Some(&content));
                Effect::Group(view)
            }
            MaterialStyle::Group { .. } => Effect::Plain,
        };
        clip.addSubview(effect.view().unwrap_or(&content));
        Self {
            source: source.clone(),
            clip,
            content,
            effect,
        }
    }

    pub(crate) fn update(
        &mut self,
        source: &NativeMaterial,
        scale: f32,
        parent_origin: Point<Pixels>,
    ) -> Result<()> {
        let bounds = source.bounds.map(|value| gpui::px(value.0 / scale));
        let mask = source
            .content_mask
            .bounds
            .map(|value| gpui::px(value.0 / scale));
        self.clip.setFrame(native_view::rect(Bounds::new(
            mask.origin - parent_origin,
            mask.size,
        )));
        self.clip.setBoundsSize(native_view::rect(mask).size);
        self.clip.setAlphaValue(source.opacity as f64);
        self.clip
            .set_mask(source.content_mask, mask.origin, scale)?;
        let content_bounds = Bounds::new(Point::default(), bounds.size);
        if let Some(effect) = self.effect.view() {
            effect.setFrame(native_view::rect(Bounds::new(
                bounds.origin - mask.origin,
                bounds.size,
            )));
            self.content.setFrame(native_view::rect(content_bounds));
        } else {
            self.content.setFrame(native_view::rect(Bounds::new(
                bounds.origin - mask.origin,
                bounds.size,
            )));
        }
        self.content
            .setBoundsSize(native_view::rect(content_bounds).size);
        match (&self.effect, source.style) {
            (
                Effect::Glass(view),
                MaterialStyle::Glass {
                    clear,
                    tint,
                    corner_radius,
                    interactive,
                },
            ) => {
                view.setStyle(if clear {
                    NSGlassEffectViewStyle::Clear
                } else {
                    NSGlassEffectViewStyle::Regular
                });
                view.setCornerRadius(f64::from(corner_radius));
                let tint = tint.map(|color| {
                    let color = Rgba::from(color);
                    NSColor::colorWithSRGBRed_green_blue_alpha(
                        color.r as f64,
                        color.g as f64,
                        color.b as f64,
                        color.a as f64,
                    )
                });
                view.setTintColor(tint.as_deref());
                if view.respondsToSelector(sel!(setEffectIsInteractive:)) {
                    // SAFETY: this public selector is checked for runtime availability.
                    unsafe {
                        let _: () = msg_send![&**view, setEffectIsInteractive: interactive];
                    }
                }
            }
            (Effect::Group(view), MaterialStyle::Group { spacing }) => {
                view.setSpacing(f64::from(spacing))
            }
            (
                Effect::Legacy(view),
                MaterialStyle::Glass { corner_radius, .. }
                | MaterialStyle::Popover { corner_radius },
            ) => {
                if let Some(layer) = view.layer() {
                    layer.setCornerRadius(f64::from(corner_radius));
                    layer.setMasksToBounds(true);
                }
            }
            _ => {}
        }
        if let Some((state, _)) = self.content.ivars().borrow_mut().material.as_mut() {
            *state = source.state.clone();
        }
        self.source = source.clone();
        self.content.report_appearance();
        Ok(())
    }
}

impl Drop for MaterialHost {
    fn drop(&mut self) {
        self.clip.removeFromSuperview();
    }
}
