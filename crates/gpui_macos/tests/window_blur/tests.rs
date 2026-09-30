use super::*;

#[test]
fn backdrop_radius_updates_nested_filters_without_changing_layer_opacity() {
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let root: id = msg_send![class!(CALayer), layer];
        let backdrop: id = msg_send![class!(CALayer), layer];
        let blur: id = msg_send![class!(CIFilter), filterWithName: ns_string("CIGaussianBlur")];
        assert!(!blur.is_null());
        let filters = NSArray::arrayWithObject(nil, blur);
        let _: () = msg_send![backdrop, setFilters: filters];
        let _: () = msg_send![root, addSublayer: backdrop];

        for radius in [60.0, 24.0, 0.0, 42.0] {
            remove_layer_background(root, radius);
            let value: id = msg_send![blur, valueForKey: ns_string("inputRadius")];
            let actual: f64 = msg_send![value, doubleValue];
            assert_eq!(actual, radius);
            let opacity: f32 = msg_send![root, opacity];
            assert_eq!(opacity, 1.0);
        }
        pool.drain();
    }
}
