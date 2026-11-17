//! Minimal late-bound COM automation (IDispatch by name), the Rust equivalent of pywin32's
//! `Dispatch`. Only used on the Outlook COM thread.

use chrono::{Duration, NaiveDate, NaiveDateTime};
use std::mem::ManuallyDrop;
use windows::core::{Interface, BSTR, GUID, IUnknown, PCWSTR};
use windows::Win32::Foundation::{VARIANT_BOOL, VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::System::Com::{
    IDispatch, DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET, DISPATCH_PROPERTYPUT, DISPPARAMS, EXCEPINFO,
};
use windows::Win32::System::Variant::*;

const LOCALE_USER_DEFAULT: u32 = 0x0400;
const DISPID_PROPERTYPUT: i32 = -3;

/// A VARIANT we own: `VariantClear` on drop releases its BSTR / interface pointer.
#[repr(transparent)]
struct OwnedVariant(VARIANT);

impl Drop for OwnedVariant {
    fn drop(&mut self) {
        unsafe {
            let _ = VariantClear(&mut self.0);
        }
    }
}

impl OwnedVariant {
    fn empty() -> Self {
        OwnedVariant(VARIANT::default()) // all zero = VT_EMPTY
    }

    fn with(vt: VARENUM, fill: impl FnOnce(&mut VARIANT_0_0_0)) -> Self {
        let mut v = VARIANT::default();
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = vt;
            fill(&mut inner.Anonymous);
        }
        OwnedVariant(v)
    }

    fn vt(&self) -> VARENUM {
        unsafe { self.0.Anonymous.Anonymous.vt }
    }

    fn data(&self) -> &VARIANT_0_0_0 {
        unsafe { &self.0.Anonymous.Anonymous.Anonymous }
    }
}

#[derive(Debug, Clone)]
pub struct ComError(pub String);

pub type ComResult<T> = Result<T, ComError>;

/// A value coming back from (or going into) an automation call.
pub enum Var {
    Empty,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// OLE automation date: days since 1899-12-30, local wall-clock time.
    Date(f64),
    Str(String),
    Obj(Disp),
    /// A byte array (VT_ARRAY | VT_UI1), e.g. `RTFBody` or a binary MAPI property. Read-only.
    Bytes(Vec<u8>),
}

impl Var {
    pub fn into_obj(self) -> Option<Disp> {
        match self {
            Var::Obj(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Var::Int(i) => Some(*i),
            Var::Float(f) => Some(*f as i64),
            Var::Bool(b) => Some(*b as i64),
            Var::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn into_string(self) -> String {
        match self {
            Var::Str(s) => s,
            Var::Int(i) => i.to_string(),
            Var::Float(f) => f.to_string(),
            Var::Bool(b) => b.to_string(),
            _ => String::new(),
        }
    }

    fn to_variant(&self) -> OwnedVariant {
        match self {
            Var::Empty => OwnedVariant::empty(),
            Var::Bool(b) => OwnedVariant::with(VT_BOOL, |d| d.boolVal = if *b { VARIANT_TRUE } else { VARIANT_FALSE }),
            Var::Int(i) => match i32::try_from(*i) {
                Ok(v) => OwnedVariant::with(VT_I4, |d| d.lVal = v),
                Err(_) => OwnedVariant::with(VT_I8, |d| d.llVal = *i),
            },
            Var::Float(f) => OwnedVariant::with(VT_R8, |d| d.dblVal = *f),
            Var::Date(f) => OwnedVariant::with(VT_DATE, |d| d.date = *f),
            Var::Str(s) => OwnedVariant::with(VT_BSTR, |d| d.bstrVal = ManuallyDrop::new(BSTR::from(s.as_str()))),
            // clone() = AddRef; VariantClear releases it.
            Var::Obj(o) => OwnedVariant::with(VT_DISPATCH, |d| d.pdispVal = ManuallyDrop::new(Some(o.0.clone()))),
            Var::Bytes(_) => OwnedVariant::empty(), // never passed to Outlook
        }
    }

    fn from_variant(v: &OwnedVariant) -> Var {
        let vt = v.vt();
        let d = v.data();
        // SAFETY: each arm reads the union field that `vt` says is active.
        unsafe {
            match vt {
                VT_EMPTY | VT_NULL | VT_ERROR => Var::Empty,
                VT_BOOL => Var::Bool(d.boolVal != VARIANT_BOOL(0)),
                VT_I1 => Var::Int(d.cVal as i64),
                VT_I2 => Var::Int(d.iVal as i64),
                VT_I4 | VT_INT => Var::Int(d.lVal as i64),
                VT_I8 => Var::Int(d.llVal),
                VT_UI1 => Var::Int(d.bVal as i64),
                VT_UI2 => Var::Int(d.uiVal as i64),
                VT_UI4 | VT_UINT => Var::Int(d.ulVal as i64),
                VT_UI8 => Var::Int(d.ullVal as i64),
                VT_R4 => Var::Float(d.fltVal as f64),
                VT_R8 => Var::Float(d.dblVal),
                VT_DATE => Var::Date(d.date),
                VT_BSTR => Var::Str(d.bstrVal.to_string()),
                VT_DISPATCH => match &*d.pdispVal {
                    Some(p) => Var::Obj(Disp(p.clone())),
                    None => Var::Empty, // VB "Nothing"
                },
                VT_UNKNOWN => match (*d.punkVal).as_ref().and_then(|u: &IUnknown| u.cast::<IDispatch>().ok()) {
                    Some(p) => Var::Obj(Disp(p)),
                    None => Var::Empty,
                },
                v if v.0 == VT_ARRAY.0 | VT_UI1.0 => Var::Bytes(safearray_bytes(d.parray)),
                _ => Var::Empty,
            }
        }
    }
}

/// Copy a one-dimensional SAFEARRAY of bytes.
unsafe fn safearray_bytes(sa: *mut windows::Win32::System::Com::SAFEARRAY) -> Vec<u8> {
    use windows::Win32::System::Ole::{SafeArrayAccessData, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayUnaccessData};
    if sa.is_null() {
        return Vec::new();
    }
    let (Ok(lo), Ok(hi)) = (SafeArrayGetLBound(sa, 1), SafeArrayGetUBound(sa, 1)) else { return Vec::new() };
    let n = (hi - lo + 1).max(0) as usize;
    let mut p: *mut std::ffi::c_void = std::ptr::null_mut();
    if n == 0 || SafeArrayAccessData(sa, &mut p).is_err() || p.is_null() {
        return Vec::new();
    }
    let out = std::slice::from_raw_parts(p as *const u8, n).to_vec();
    let _ = SafeArrayUnaccessData(sa);
    out
}

pub fn ole_date(d: f64) -> Option<NaiveDateTime> {
    if !d.is_finite() || d < 0.0 {
        return None;
    }
    let base = NaiveDate::from_ymd_opt(1899, 12, 30)?.and_hms_opt(0, 0, 0)?;
    base.checked_add_signed(Duration::milliseconds((d * 86_400_000.0).round() as i64))
}

#[derive(Clone)]
pub struct Disp(pub IDispatch);

impl Disp {
    fn dispid(&self, name: &str) -> ComResult<i32> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let names = [PCWSTR(wide.as_ptr())];
        let mut id = 0i32;
        unsafe { self.0.GetIDsOfNames(&GUID::zeroed(), names.as_ptr(), 1, LOCALE_USER_DEFAULT, &mut id) }
            .map_err(|e| ComError(format!("'{name}' is not available on this Outlook object ({})", e.message())))?;
        Ok(id)
    }

    fn invoke(&self, name: &str, flags: DISPATCH_FLAGS, args: &[Var], put: bool) -> ComResult<Var> {
        // Hard rule: this app never sends mail. Refuse at the lowest level, whatever the caller.
        if name.eq_ignore_ascii_case("send") {
            return Err(ComError("Sending email is not allowed in this app.".into()));
        }
        let id = self.dispid(name)?;
        // Automation expects arguments in reverse order.
        let mut vargs: Vec<OwnedVariant> = args.iter().rev().map(Var::to_variant).collect();
        let mut named = DISPID_PROPERTYPUT;
        let params = DISPPARAMS {
            // OwnedVariant is #[repr(transparent)] over VARIANT.
            rgvarg: if vargs.is_empty() { std::ptr::null_mut() } else { vargs.as_mut_ptr() as *mut VARIANT },
            rgdispidNamedArgs: if put { &mut named } else { std::ptr::null_mut() },
            cArgs: vargs.len() as u32,
            cNamedArgs: if put { 1 } else { 0 },
        };
        let mut result = OwnedVariant::empty();
        let mut excep = EXCEPINFO::default();
        let mut arg_err = 0u32;
        let r = unsafe {
            self.0.Invoke(id, &GUID::zeroed(), LOCALE_USER_DEFAULT, flags, &params, Some(&mut result.0),
                          Some(&mut excep), Some(&mut arg_err))
        };
        // EXCEPINFO's strings are ours to free.
        let (desc, _src, _help) = unsafe {
            (ManuallyDrop::take(&mut excep.bstrDescription), ManuallyDrop::take(&mut excep.bstrSource),
             ManuallyDrop::take(&mut excep.bstrHelpFile))
        };
        match r {
            Ok(()) => Ok(Var::from_variant(&result)),
            Err(e) => {
                let desc = desc.to_string();
                let msg = if desc.is_empty() { e.message().to_string() } else { desc };
                Err(ComError(format!("Outlook: {name} failed: {}", msg.trim())))
            }
        }
    }

    pub fn get(&self, name: &str) -> ComResult<Var> {
        self.invoke(name, DISPATCH_PROPERTYGET | DISPATCH_METHOD, &[], false)
    }

    pub fn call(&self, name: &str, args: &[Var]) -> ComResult<Var> {
        self.invoke(name, DISPATCH_METHOD | DISPATCH_PROPERTYGET, args, false)
    }

    pub fn put(&self, name: &str, value: Var) -> ComResult<()> {
        self.invoke(name, DISPATCH_PROPERTYPUT, &[value], true).map(|_| ())
    }

    pub fn get_obj(&self, name: &str) -> ComResult<Disp> {
        self.get(name)?.into_obj().ok_or_else(|| ComError(format!("Outlook returned nothing for {name}")))
    }

    pub fn call_obj(&self, name: &str, args: &[Var]) -> ComResult<Disp> {
        self.call(name, args)?.into_obj().ok_or_else(|| ComError(format!("Outlook returned nothing for {name}")))
    }

    /// Like Python's getattr(obj, name, ""): errors become an empty string.
    pub fn str_or_empty(&self, name: &str) -> String {
        self.get(name).map(Var::into_string).unwrap_or_default()
    }

    pub fn i64_or(&self, name: &str, default: i64) -> i64 {
        self.get(name).ok().and_then(|v| v.as_i64()).unwrap_or(default)
    }

    pub fn bool_or_false(&self, name: &str) -> bool {
        match self.get(name) {
            Ok(Var::Bool(b)) => b,
            Ok(v) => v.as_i64().is_some_and(|i| i != 0),
            Err(_) => false,
        }
    }

    pub fn date(&self, name: &str) -> Option<NaiveDateTime> {
        match self.get(name) {
            Ok(Var::Date(d)) | Ok(Var::Float(d)) => ole_date(d),
            _ => None,
        }
    }

    /// Materialise a 1-based COM collection.
    pub fn items(&self) -> Vec<Disp> {
        let n = self.i64_or("Count", 0);
        (1..=n).filter_map(|i| self.call_obj("Item", &[Var::Int(i)]).ok()).collect()
    }
}
