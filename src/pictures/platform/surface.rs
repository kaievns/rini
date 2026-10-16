//! IOSurfaces of this process's own, written once as they are made, and a surface's bytes read under
//! its lock. The only place a surface's memory is reached.

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString};
use objc2_io_surface::{
    IOSurfaceLockOptions, IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceBytesPerRow,
    kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth,
};

/// How a new surface of four bytes a pixel lays its pixels out: the row stride and pixel format
/// IOSurface picks, unless given.
#[derive(Debug, Clone, Copy, Default)]
pub struct Layout {
    pub bytes_per_row: Option<usize>,
    pub pixel_format: Option<u32>,
}

/// A new `width` x `height` surface laid out as `layout` says, its bytes written by `fill` before
/// anything else holds it: `fill` is handed every row, `bytes_per_row` apart. `None` when IOSurface
/// makes none, the surface is smaller than its rows, or `fill` gives up.
pub fn new_surface(
    width: usize,
    height: usize,
    layout: Layout,
    fill: impl FnOnce(&mut [u8], usize) -> Option<()>,
) -> Option<CFRetained<IOSurfaceRef>> {
    // SAFETY: each is an immutable `CFString` constant exported by IOSurface.
    let [width_key, height_key, element_key, row_key, format_key]: [&CFString; 5] = unsafe {
        [
            kIOSurfaceWidth,
            kIOSurfaceHeight,
            kIOSurfaceBytesPerElement,
            kIOSurfaceBytesPerRow,
            kIOSurfacePixelFormat,
        ]
    };
    let number = |value: usize| CFNumber::new_i64(value as i64);
    let mut keys = vec![width_key, height_key, element_key];
    let mut values = vec![number(width), number(height), number(4)];
    if let Some(stride) = layout.bytes_per_row {
        keys.push(row_key);
        values.push(number(stride));
    }
    if let Some(format) = layout.pixel_format {
        keys.push(format_key);
        values.push(number(format as usize));
    }
    let value_refs: Vec<&CFNumber> = values.iter().map(|value| &**value).collect();
    let properties = CFDictionary::from_slices(&keys, &value_refs);
    // SAFETY: every key is an IOSurface property whose value is a `CFNumber`, and every value is one.
    let surface = unsafe { IOSurfaceRef::new(properties.as_opaque()) }?;
    let stride = surface.bytes_per_row();
    with_locked(&surface, IOSurfaceLockOptions::empty(), |base, length| {
        // SAFETY: `with_locked` hands the `length` bytes the surface holds at `base`, locked for
        // writing, and the surface is this function's alone, so nothing else reads or writes them.
        fill(unsafe { std::slice::from_raw_parts_mut(base, length) }, stride)
    })
    .flatten()?;
    Some(surface)
}

/// Hands `read` the bytes of `surface`, every row, `bytes_per_row` apart, under a read-only lock.
/// `None` when the lock fails or the surface is smaller than its rows.
pub fn read_locked<R>(surface: &IOSurfaceRef, read: impl FnOnce(&[u8]) -> R) -> Option<R> {
    with_locked(surface, IOSurfaceLockOptions::ReadOnly, |base, length| {
        // SAFETY: `with_locked` hands the `length` bytes the surface holds at `base`, locked. This
        // process writes a surface only inside `new_surface`, before anything else holds it.
        read(unsafe { std::slice::from_raw_parts(base, length) })
    })
}

/// Runs `reach` on the base address and length of `surface`'s rows while it is locked with `options`,
/// and unlocks it after. `None` when the lock fails or the surface's allocation is smaller than its
/// `bytes_per_row * height`.
fn with_locked<R>(
    surface: &IOSurfaceRef,
    options: IOSurfaceLockOptions,
    reach: impl FnOnce(*mut u8, usize) -> R,
) -> Option<R> {
    let length = surface.bytes_per_row().checked_mul(surface.height())?;
    if length > surface.alloc_size() {
        return None;
    }
    // SAFETY: a null seed is allowed, and the lock is paired with the unlock below.
    if unsafe { surface.lock(options, std::ptr::null_mut()) } != 0 {
        return None;
    }
    let reached = reach(surface.base_address().as_ptr().cast::<u8>(), length);
    // SAFETY: as for the lock, with the same options.
    unsafe { surface.unlock(options, std::ptr::null_mut()) };
    Some(reached)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a fill writes is what a read finds, row by row at the surface's stride.
    #[test]
    fn a_new_surface_holds_what_its_fill_wrote() {
        let byte = |row: usize, column: usize| (row * 31 + column) as u8;
        let surface = new_surface(7, 5, Layout::default(), |bytes, stride| {
            for row in 0..5 {
                for column in 0..7 * 4 {
                    bytes[row * stride + column] = byte(row, column);
                }
            }
            Some(())
        })
        .expect("a surface");
        assert_eq!((surface.width(), surface.height()), (7, 5));
        let stride = surface.bytes_per_row();
        let read = read_locked(&surface, |bytes| {
            (0..5).all(|row| {
                (0..7 * 4).all(|column| bytes[row * stride + column] == byte(row, column))
            })
        });
        assert_eq!(read, Some(true));
    }

    /// A surface is made with the stride and pixel format it is given.
    #[test]
    fn a_new_surface_takes_the_layout_it_is_given() {
        let layout = Layout {
            bytes_per_row: Some(64),
            pixel_format: Some(u32::from_be_bytes(*b"BGRA")),
        };
        let surface = new_surface(3, 2, layout, |_, _| Some(())).expect("a surface");
        assert_eq!(surface.bytes_per_row(), 64);
        assert_eq!(surface.pixel_format(), u32::from_be_bytes(*b"BGRA"));
    }

    /// A fill that gives up leaves no surface behind.
    #[test]
    fn a_fill_that_gives_up_makes_no_surface() {
        assert!(new_surface(4, 4, Layout::default(), |_, _| None).is_none());
    }
}
