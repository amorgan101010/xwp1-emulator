//! glibc 2.43 gave `atan2f` and `acosf` new symbol versions, so a plugin
//! built on a current system would not load in a host that brings an older
//! glibc (Bitwig's Flatpak runtime has 2.35). They are only used by the
//! little window's drawing: answer them here instead of importing them.

#[no_mangle]
pub extern "C" fn atan2f(y: f32, x: f32) -> f32 {
    libm::atan2f(y, x)
}

#[no_mangle]
pub extern "C" fn acosf(x: f32) -> f32 {
    libm::acosf(x)
}
