use std::error::Error as StdError;
use std::ffi::c_void;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::ptr::{self, NonNull};

use objc2_application_services::{AXError, AXUIElement as RawAXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize, ConcreteType,
};
use rini_core::ids::pid_t;

pub const AX_WINDOW_ROLE: &str = "AXWindow";
pub const AX_STANDARD_WINDOW_SUBROLE: &str = "AXStandardWindow";

#[derive(Clone)]
pub struct AXUIElement {
    inner: CFRetained<RawAXUIElement>,
}

#[derive(Debug, Clone)]
pub enum Error {
    Ax(AXError),
    NotFound,
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Ax(err) => write!(f, "AX error {err:?}"),
            Error::NotFound => write!(f, "value not found"),
        }
    }
}

impl StdError for Error {}

impl From<AXError> for Error {
    fn from(value: AXError) -> Self { Self::Ax(value) }
}

impl AXUIElement {
    fn new(inner: CFRetained<RawAXUIElement>) -> Self { Self { inner } }

    #[inline]
    pub fn application(pid: pid_t) -> Self {
        // SAFETY: takes only a pid; the element comes back +1 under the Create rule, in a
        // `CFRetained`.
        let inner = unsafe { RawAXUIElement::new_application(pid) };
        Self::new(inner)
    }

    #[allow(non_snake_case, reason = "core-foundation's method name")]
    #[inline]
    pub fn as_concrete_TypeRef(&self) -> &RawAXUIElement { self.deref() }

    #[inline]
    pub fn raw_ptr(&self) -> NonNull<RawAXUIElement> { CFRetained::as_ptr(&self.inner) }

    /// # Safety
    ///
    /// `ptr` is a live `AXUIElement` the caller does not own; this takes a retain of its own.
    #[inline]
    pub unsafe fn from_get_rule(ptr: *const RawAXUIElement) -> Self {
        let ptr = NonNull::new(ptr.cast_mut()).expect("attempted to create a NULL object");
        // SAFETY: the caller's: `ptr` is a live element; the retain taken here is ours.
        let retained = unsafe { CFRetained::retain(ptr) };
        Self::new(retained)
    }

    fn copy_attribute(&self, name: &'static str) -> Result<Option<CFRetained<CFType>>> {
        let attr = CFString::from_static_str(name);
        let mut value: *const CFType = ptr::null();
        // SAFETY: `attr` is live for the call, and `value` is a local it writes.
        let status = unsafe {
            self.inner.copy_attribute_value(
                attr.as_ref(),
                NonNull::new((&mut value) as *mut *const CFType)
                    .expect("pointer to local is never null"),
            )
        };
        match status {
            AXError::Success => {
                if value.is_null() {
                    Ok(None)
                } else {
                    // SAFETY: a Copy call that succeeded, so `value` is +1 and the `CFRetained`
                    // takes that retain.
                    let retained = unsafe {
                        CFRetained::from_raw(
                            NonNull::new(value as *mut CFType).expect("non-null value pointer"),
                        )
                    };
                    Ok(Some(retained))
                }
            }
            AXError::NoValue => Ok(None),
            err => Err(Error::Ax(err)),
        }
    }

    fn copy_required_attribute(&self, name: &'static str) -> Result<CFRetained<CFType>> {
        self.copy_attribute(name)?.ok_or(Error::NotFound)
    }

    fn downcast<T: ConcreteType>(&self, value: CFRetained<CFType>) -> Result<CFRetained<T>> {
        value.downcast::<T>().map_err(|_| Error::Ax(AXError::Failure))
    }

    pub fn bool_attribute(&self, name: &'static str) -> Result<bool> {
        let value = self.copy_required_attribute(name)?;
        let boolean = self.downcast::<CFBoolean>(value)?;
        Ok(boolean.value())
    }

    pub fn is_settable(&self, name: &'static str) -> Result<bool> {
        let mut is_settable: u8 = 0;
        // SAFETY: the name is live for the call, and `is_settable` is a local `Boolean` it writes.
        let status = unsafe {
            self.inner.is_attribute_settable(
                CFString::from_static_str(name).as_ref(),
                NonNull::from(&mut is_settable),
            )
        };
        match status {
            AXError::Success => Ok(is_settable != 0),
            err => Err(Error::Ax(err)),
        }
    }

    pub fn frame(&self) -> Result<CGRect> {
        let value = self.copy_required_attribute("AXFrame")?;
        let ax_value = self.downcast::<AXValue>(value)?;
        ax_struct(&ax_value)
    }

    pub fn role(&self) -> Result<String> {
        let value = self.copy_required_attribute("AXRole")?;
        let string = self.downcast::<CFString>(value)?;
        Ok(string.to_string())
    }

    pub fn subrole(&self) -> Result<String> {
        let value = self.copy_required_attribute("AXSubrole")?;
        let string = self.downcast::<CFString>(value)?;
        Ok(string.to_string())
    }

    pub fn minimized(&self) -> Result<bool> { self.bool_attribute("AXMinimized") }

    /// `kAXModalAttribute`: whether the window is modal. Most windows do not expose the
    /// attribute at all, which comes back as an error and is read as "not modal".
    pub fn modal(&self) -> Result<bool> { self.bool_attribute("AXModal") }

    pub fn title(&self) -> Result<String> {
        let value = self.copy_required_attribute("AXTitle")?;
        let string = self.downcast::<CFString>(value)?;
        Ok(string.to_string())
    }

    pub fn frontmost(&self) -> Result<bool> { self.bool_attribute("AXFrontmost") }

    pub fn main_window(&self) -> Result<AXUIElement> {
        let value = self.copy_required_attribute("AXMainWindow")?;
        let element = self.downcast::<RawAXUIElement>(value)?;
        Ok(AXUIElement::new(element))
    }

    pub fn windows(&self) -> Result<Vec<AXUIElement>> {
        let Some(value) = self.copy_attribute("AXWindows")? else {
            return Ok(Vec::new());
        };
        let array = self.downcast::<CFArray>(value)?;
        // SAFETY: every entry of a CFArray is a CF object, and each is downcast before it is used.
        let array = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(array) };
        let mut out = Vec::with_capacity(array.len());
        for entry in array.iter() {
            let elem = self.downcast::<RawAXUIElement>(entry)?;
            out.push(AXUIElement::new(elem));
        }
        Ok(out)
    }

    pub fn attribute(&self, name: &'static str) -> Result<Option<CFRetained<CFType>>> {
        self.copy_attribute(name)
    }

    pub fn set_position(&self, point: CGPoint) -> Result<()> {
        let attr = CFString::from_static_str("AXPosition");
        let value = make_axvalue(&point)?;
        self.set_attribute_value(attr.as_ref(), value.as_ref())
    }

    pub fn set_size(&self, size: CGSize) -> Result<()> {
        let attr = CFString::from_static_str("AXSize");
        let value = make_axvalue(&size)?;
        self.set_attribute_value(attr.as_ref(), value.as_ref())
    }

    pub fn raise(&self) -> Result<()> {
        let action = CFString::from_static_str("AXRaise");
        // SAFETY: `action` is live for the call.
        let status = unsafe { self.inner.perform_action(action.as_ref()) };
        if status == AXError::Success {
            Ok(())
        } else {
            Err(Error::Ax(status))
        }
    }

    fn set_attribute_value(&self, name: &CFString, value: &CFType) -> Result<()> {
        // SAFETY: `name` and `value` are live CF objects, borrowed for the call.
        let status = unsafe { self.inner.set_attribute_value(name, value) };
        if status == AXError::Success {
            Ok(())
        } else {
            Err(Error::Ax(status))
        }
    }

    pub fn set_bool_attribute(&self, name: &'static str, value: bool) -> Result<()> {
        let cf_bool = CFBoolean::new(value);
        let attr = CFString::from_static_str(name);
        self.set_attribute_value(attr.as_ref(), cf_bool.as_ref())
    }

    pub fn can_resize(&self) -> Result<bool> { self.is_settable("AXSize") }
}

impl Deref for AXUIElement {
    type Target = RawAXUIElement;

    fn deref(&self) -> &Self::Target { &self.inner }
}

impl PartialEq for AXUIElement {
    fn eq(&self, other: &Self) -> bool {
        // AX APIs may return a fresh retained wrapper for the same remote
        // accessibility object. The generated Core Foundation type implements
        // equality with CFEqual, which compares that remote identity; comparing
        // allocation addresses makes AXWindows and AXMainWindow disagree after
        // wake even when they refer to the same window.
        self.deref().eq(other.deref())
    }
}

impl Eq for AXUIElement {}

impl Hash for AXUIElement {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Keep Hash consistent with the CFEqual-based PartialEq implementation.
        self.deref().hash(state);
    }
}

impl fmt::Debug for AXUIElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.deref().fmt(f) }
}

/// A struct an `AXValue` holds, and the `AXValueType` that names it.
///
/// # Safety
///
/// `TYPE` names exactly `Self`'s C struct, since `AXValueGetValue` and `AXValueCreate` copy that
/// type's size through a pointer to `Self`.
unsafe trait AxStruct: Default {
    const TYPE: AXValueType;
}

// SAFETY: `kAXValueCGPointType` is a `CGPoint`.
unsafe impl AxStruct for CGPoint {
    const TYPE: AXValueType = AXValueType::CGPoint;
}

// SAFETY: `kAXValueCGSizeType` is a `CGSize`.
unsafe impl AxStruct for CGSize {
    const TYPE: AXValueType = AXValueType::CGSize;
}

// SAFETY: `kAXValueCGRectType` is a `CGRect`.
unsafe impl AxStruct for CGRect {
    const TYPE: AXValueType = AXValueType::CGRect;
}

/// The `T` `value` holds; an error when it holds another type.
fn ax_struct<T: AxStruct>(value: &AXValue) -> Result<T> {
    let mut out = T::default();
    // SAFETY: `out` is a `T`, the struct `T::TYPE` names, so the call writes no more than it holds.
    let success = unsafe { value.value(T::TYPE, NonNull::from(&mut out).cast::<c_void>()) };
    if success {
        Ok(out)
    } else {
        Err(Error::Ax(AXError::Failure))
    }
}

fn make_axvalue<T: AxStruct>(value: &T) -> Result<CFRetained<AXValue>> {
    // SAFETY: `value` is a `T`, the struct `T::TYPE` names, so the call reads no more than it holds.
    unsafe { AXValue::new(T::TYPE, NonNull::from(value).cast::<c_void>()) }
        .ok_or(Error::Ax(AXError::Failure))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn separately_retained_elements_share_logical_identity() {
        let pid = std::process::id() as pid_t;
        let first = AXUIElement::application(pid);
        let second = AXUIElement::application(pid);

        let mut by_element = HashMap::new();
        by_element.insert(first, 1);

        assert_eq!(by_element.get(&second), Some(&1));
    }

    #[test]
    fn an_ax_value_reads_back_the_struct_it_was_made_of_and_no_other() {
        let point = CGPoint::new(12.0, -3.5);
        let size = CGSize::new(640.0, 480.0);
        let made = make_axvalue(&point).expect("a point value");
        assert_eq!(ax_struct::<CGPoint>(&made).ok(), Some(point));
        assert!(
            ax_struct::<CGRect>(&made).is_err(),
            "a point is not read as a rect"
        );
        let made = make_axvalue(&size).expect("a size value");
        assert_eq!(ax_struct::<CGSize>(&made).ok(), Some(size));
    }
}
