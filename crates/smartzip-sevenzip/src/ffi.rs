//! Private ABI for official 7-Zip 26.06, using 32-bit HRESULT/ULONG on every target.
use std::{ffi::c_void, ptr};
pub type Hr = i32;
pub const OK: Hr = 0;
pub const FALSE: Hr = 1;
pub const FAIL: Hr = 0x80004005u32 as i32;
pub const ABORT: Hr = 0x80004004u32 as i32;
pub const NO_INTERFACE: Hr = 0x80004002u32 as i32;
#[cfg(windows)]
pub type Wide = u16;
#[cfg(not(windows))]
pub type Wide = u32;
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid {
    pub a: u32,
    pub b: u16,
    pub c: u16,
    pub d: [u8; 8],
}
pub const fn iid(group: u8, id: u8) -> Guid {
    Guid {
        a: 0x23170f69,
        b: 0x40c1,
        c: 0x278a,
        d: [0, 0, 0, group, 0, id, 0, 0],
    }
}
pub const UNKNOWN: Guid = Guid {
    a: 0,
    b: 0,
    c: 0,
    d: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Counted {
    pub count: u32,
    pub pointer: *mut c_void,
}
#[repr(C)]
pub union Value {
    pub number: u64,
    pub text: *mut Wide,
    #[cfg(windows)]
    pub counted: Counted,
}
#[repr(C)]
pub struct Variant {
    pub kind: u16,
    pub reserved: [u16; 3],
    pub value: Value,
}
impl Variant {
    pub fn empty() -> Self {
        Self {
            kind: 0,
            reserved: [0; 3],
            value: Value { number: 0 },
        }
    }
}
pub type Query = unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> Hr;
pub type Ref = unsafe extern "system" fn(*mut c_void) -> u32;
#[repr(C)]
pub struct Unknown {
    pub query: Query,
    pub add: Ref,
    pub release: Ref,
}
#[repr(C)]
pub struct InputVtable {
    pub base: Unknown,
    pub read: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut u32) -> Hr,
    pub seek: unsafe extern "system" fn(*mut c_void, i64, u32, *mut u64) -> Hr,
}
#[repr(C)]
pub struct OutputVtable {
    pub base: Unknown,
    pub write: unsafe extern "system" fn(*mut c_void, *const c_void, u32, *mut u32) -> Hr,
}
#[repr(C)]
pub struct OpenVtable {
    pub base: Unknown,
    pub total: unsafe extern "system" fn(*mut c_void, *const u64, *const u64) -> Hr,
    pub completed: unsafe extern "system" fn(*mut c_void, *const u64, *const u64) -> Hr,
}
#[repr(C)]
pub struct VolumeVtable {
    pub base: Unknown,
    pub property: unsafe extern "system" fn(*mut c_void, u32, *mut Variant) -> Hr,
    pub stream: unsafe extern "system" fn(*mut c_void, *const Wide, *mut *mut c_void) -> Hr,
}
#[repr(C)]
pub struct PasswordVtable {
    pub base: Unknown,
    pub password: unsafe extern "system" fn(*mut c_void, *mut *mut Wide) -> Hr,
}
#[repr(C)]
pub struct MessageVtable {
    pub base: Unknown,
    pub result: unsafe extern "system" fn(*mut c_void, u32, u32, i32) -> Hr,
}
#[repr(C)]
pub struct ExtractVtable {
    pub base: Unknown,
    pub total: unsafe extern "system" fn(*mut c_void, u64) -> Hr,
    pub completed: unsafe extern "system" fn(*mut c_void, *const u64) -> Hr,
    pub stream: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void, i32) -> Hr,
    pub prepare: unsafe extern "system" fn(*mut c_void, i32) -> Hr,
    pub result: unsafe extern "system" fn(*mut c_void, i32) -> Hr,
}
#[repr(C)]
pub struct ArchiveVtable {
    pub base: Unknown,
    pub open: unsafe extern "system" fn(*mut c_void, *mut c_void, *const u64, *mut c_void) -> Hr,
    pub close: unsafe extern "system" fn(*mut c_void) -> Hr,
    pub count: unsafe extern "system" fn(*mut c_void, *mut u32) -> Hr,
    pub property: unsafe extern "system" fn(*mut c_void, u32, u32, *mut Variant) -> Hr,
    pub extract: unsafe extern "system" fn(*mut c_void, *const u32, u32, i32, *mut c_void) -> Hr,
    pub archive_property: unsafe extern "system" fn(*mut c_void, u32, *mut Variant) -> Hr,
}
pub type Create = unsafe extern "system" fn(*const Guid, *const Guid, *mut *mut c_void) -> Hr;
pub type Count = unsafe extern "system" fn(*mut u32) -> Hr;
pub type Property = unsafe extern "system" fn(u32, u32, *mut Variant) -> Hr;
pub type ModuleProperty = unsafe extern "system" fn(u32, *mut Variant) -> Hr;
pub type Clear = unsafe extern "system" fn(*mut Variant) -> Hr;
pub type Length = unsafe extern "system" fn(*mut Wide) -> u32;
pub type Alloc = unsafe extern "system" fn(*const Wide, u32) -> *mut Wide;
#[cfg(windows)]
#[link(name = "oleaut32")]
unsafe extern "system" {
    pub fn VariantClear(value: *mut Variant) -> Hr;
    pub fn SysStringByteLen(value: *mut Wide) -> u32;
    pub fn SysAllocStringLen(value: *const Wide, len: u32) -> *mut Wide;
}

/// An owned COM reference. Its creator keeps the dynamic library alive.
pub struct Com(pub *mut c_void);
impl Com {
    pub unsafe fn table<T>(&self) -> &T {
        &**self.0.cast::<*const T>()
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                (self.table::<Unknown>().release)(self.0);
            }
        }
    }
}
pub fn wide(text: &str) -> Vec<Wide> {
    #[cfg(windows)]
    {
        text.encode_utf16().collect()
    }
    #[cfg(not(windows))]
    {
        text.chars().map(|c| c as u32).collect()
    }
}
pub fn decode(chars: &[Wide]) -> String {
    #[cfg(windows)]
    {
        String::from_utf16_lossy(chars)
    }
    #[cfg(not(windows))]
    {
        // Unix 7-Zip can carry UTF-16 surrogate pairs in wchar_t strings too.
        let mut result = String::new();
        let mut iter = chars.iter().copied();
        while let Some(c) = iter.next() {
            if (0xd800..=0xdbff).contains(&c) {
                match iter.next() {
                    Some(d) if (0xdc00..=0xdfff).contains(&d) => result.push(
                        char::from_u32(0x10000 + ((c - 0xd800) << 10) + (d - 0xdc00))
                            .unwrap_or('\u{fffd}'),
                    ),
                    Some(d) => {
                        result.push('\u{fffd}');
                        result.push(char::from_u32(d).unwrap_or('\u{fffd}'));
                    }
                    None => result.push('\u{fffd}'),
                }
            } else {
                result.push(char::from_u32(c).unwrap_or('\u{fffd}'));
            }
        }
        result
    }
}
pub unsafe fn nul_text(text: *const Wide) -> String {
    if text.is_null() {
        return String::new();
    }
    let mut n = 0;
    while ptr::read(text.add(n)) != 0 {
        n += 1;
    }
    decode(std::slice::from_raw_parts(text, n))
}
