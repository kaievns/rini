//! Accessibility, the window server, Carbon, and the actors that drive them.

pub mod app;
pub mod app_actor;
pub mod attention;
pub mod ax;
pub mod carbon;
pub mod lifecycle;
pub mod mouse;
pub mod process;
pub mod sub_level;
pub mod window_server;

#[inline(always)]
pub(crate) fn cg_ok(err: objc2_core_graphics::CGError) -> Result<(), objc2_core_graphics::CGError> {
    if err == objc2_core_graphics::CGError::Success {
        Ok(())
    } else {
        Err(err)
    }
}

/// The address of `name` in any image loaded into the process, none when no image exports it.
pub(crate) fn symbol(name: &std::ffi::CStr) -> Option<std::ptr::NonNull<std::ffi::c_void>> {
    // SAFETY: `name` is NUL-terminated, and `RTLD_DEFAULT` searches the loaded images, changing none.
    std::ptr::NonNull::new(unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) })
}
