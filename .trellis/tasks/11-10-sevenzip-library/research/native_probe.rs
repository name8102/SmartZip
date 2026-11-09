//! Linux x86-64 research spike: direct Rust FFI, no C++ compilation/linkage.
//! Only plain archives; password/multi-interface callbacks belong to implementation.
use std::{
    ffi::{c_char, c_int, c_void, CString},
    fs::File,
    io::{Read, Seek, SeekFrom},
    ptr,
    sync::{
        atomic::{fence, AtomicU32, AtomicU64, Ordering},
        Mutex,
    },
};

type Hr = i32;
const OK: Hr = 0;
const FAIL: Hr = 0x80004005u32 as Hr;
const ABORT: Hr = 0x80004004u32 as Hr;
const NO_INTERFACE: Hr = 0x80004002u32 as Hr;
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct Guid {
    a: u32,
    b: u16,
    c: u16,
    d: [u8; 8],
}
const fn iid(group: u8, id: u8) -> Guid {
    Guid {
        a: 0x23170f69,
        b: 0x40c1,
        c: 0x278a,
        d: [0, 0, 0, group, 0, id, 0, 0],
    }
}
const UNKNOWN: Guid = Guid {
    a: 0,
    b: 0,
    c: 0,
    d: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
#[repr(C)]
union Value {
    number: u64,
    text: *mut u32,
}
#[repr(C)]
struct Variant {
    kind: u16,
    reserved: [u16; 3],
    value: Value,
}
impl Variant {
    fn empty() -> Self {
        Self {
            kind: 0,
            reserved: [0; 3],
            value: Value { number: 0 },
        }
    }
}

type Query = unsafe extern "C" fn(*mut c_void, *const Guid, *mut *mut c_void) -> Hr;
type Ref = unsafe extern "C" fn(*mut c_void) -> u32;
#[repr(C)]
struct Unknown {
    query: Query,
    add: Ref,
    release: Ref,
}
#[repr(C)]
struct InputVtable {
    base: Unknown,
    read: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut u32) -> Hr,
    seek: unsafe extern "C" fn(*mut c_void, i64, u32, *mut u64) -> Hr,
}
#[repr(C)]
struct Input {
    vtable: *const InputVtable,
    refs: AtomicU32,
    file: Mutex<File>,
}
unsafe extern "C" fn input_query(this: *mut c_void, id: *const Guid, out: *mut *mut c_void) -> Hr {
    *out = ptr::null_mut();
    if *id != UNKNOWN && *id != iid(3, 1) && *id != iid(3, 3) {
        return NO_INTERFACE;
    }
    *out = this;
    input_add(this);
    OK
}
unsafe extern "C" fn input_add(this: *mut c_void) -> u32 {
    (*this.cast::<Input>()).refs.fetch_add(1, Ordering::Relaxed) + 1
}
unsafe extern "C" fn input_release(this: *mut c_void) -> u32 {
    let left = (*this.cast::<Input>()).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        fence(Ordering::Acquire);
        drop(Box::from_raw(this.cast::<Input>()));
    }
    left
}
unsafe extern "C" fn input_read(
    this: *mut c_void,
    data: *mut c_void,
    size: u32,
    done: *mut u32,
) -> Hr {
    if !done.is_null() {
        *done = 0;
    }
    if size == 0 {
        return OK;
    }
    let Ok(mut file) = (*this.cast::<Input>()).file.lock() else {
        return FAIL;
    };
    match file.read(std::slice::from_raw_parts_mut(data.cast(), size as usize)) {
        Ok(n) => {
            if !done.is_null() {
                *done = n as u32;
            }
            OK
        }
        Err(_) => FAIL,
    }
}
unsafe extern "C" fn input_seek(
    this: *mut c_void,
    offset: i64,
    origin: u32,
    position: *mut u64,
) -> Hr {
    let from = match origin {
        0 if offset >= 0 => SeekFrom::Start(offset as u64),
        1 => SeekFrom::Current(offset),
        2 => SeekFrom::End(offset),
        _ => return FAIL,
    };
    let Ok(mut file) = (*this.cast::<Input>()).file.lock() else {
        return FAIL;
    };
    match file.seek(from) {
        Ok(p) => {
            if !position.is_null() {
                *position = p;
            }
            OK
        }
        Err(_) => FAIL,
    }
}
static INPUT: InputVtable = InputVtable {
    base: Unknown {
        query: input_query,
        add: input_add,
        release: input_release,
    },
    read: input_read,
    seek: input_seek,
};

#[repr(C)]
struct Face {
    vtable: *const c_void,
    owner: *mut Callback,
}
#[repr(C)]
struct ExtractVtable {
    base: Unknown,
    total: unsafe extern "C" fn(*mut c_void, u64) -> Hr,
    completed: unsafe extern "C" fn(*mut c_void, *const u64) -> Hr,
    stream: unsafe extern "C" fn(*mut c_void, u32, *mut *mut c_void, i32) -> Hr,
    prepare: unsafe extern "C" fn(*mut c_void, i32) -> Hr,
    result: unsafe extern "C" fn(*mut c_void, i32) -> Hr,
}
#[repr(C)]
struct OutputVtable {
    base: Unknown,
    write: unsafe extern "C" fn(*mut c_void, *const c_void, u32, *mut u32) -> Hr,
}
struct Callback {
    extract: Face,
    output: Face,
    refs: AtomicU32,
    cancel: bool,
    results: AtomicU32,
    failures: AtomicU32,
    bytes: AtomicU64,
}
unsafe fn owner(this: *mut c_void) -> *mut Callback {
    (*this.cast::<Face>()).owner
}
unsafe extern "C" fn callback_add(this: *mut c_void) -> u32 {
    (*owner(this)).refs.fetch_add(1, Ordering::Relaxed) + 1
}
unsafe extern "C" fn callback_release(this: *mut c_void) -> u32 {
    let state = owner(this);
    let left = (*state).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        fence(Ordering::Acquire);
        drop(Box::from_raw(state));
    }
    left
}
unsafe extern "C" fn callback_query(
    this: *mut c_void,
    id: *const Guid,
    out: *mut *mut c_void,
) -> Hr {
    *out = ptr::null_mut();
    let state = owner(this);
    if *id == UNKNOWN || *id == iid(0, 5) || *id == iid(6, 0x20) {
        *out = ptr::addr_of_mut!((*state).extract).cast();
    } else if *id == iid(3, 2) {
        *out = ptr::addr_of_mut!((*state).output).cast();
    } else {
        return NO_INTERFACE;
    }
    callback_add(*out);
    OK
}
unsafe extern "C" fn total(_: *mut c_void, _: u64) -> Hr {
    OK
}
unsafe extern "C" fn completed(this: *mut c_void, _: *const u64) -> Hr {
    if (*owner(this)).cancel {
        ABORT
    } else {
        OK
    }
}
unsafe extern "C" fn stream(this: *mut c_void, index: u32, out: *mut *mut c_void, mode: i32) -> Hr {
    *out = ptr::null_mut();
    println!("stream index={index} mode={mode}");
    if mode == 0 {
        *out = ptr::addr_of_mut!((*owner(this)).output).cast();
        callback_add(*out);
    }
    OK
}
unsafe extern "C" fn prepare(_: *mut c_void, _: i32) -> Hr {
    OK
}
unsafe extern "C" fn result(this: *mut c_void, code: i32) -> Hr {
    (*owner(this)).results.fetch_add(1, Ordering::Relaxed);
    if code != 0 {
        (*owner(this)).failures.fetch_add(1, Ordering::Relaxed);
    }
    println!("operation_result={code}");
    OK
}
unsafe extern "C" fn write(this: *mut c_void, _: *const c_void, size: u32, done: *mut u32) -> Hr {
    if !done.is_null() {
        *done = 0;
    }
    if (*owner(this)).cancel {
        return ABORT;
    }
    (*owner(this))
        .bytes
        .fetch_add(size as u64, Ordering::Relaxed);
    if !done.is_null() {
        *done = size;
    }
    OK
}
static EXTRACT: ExtractVtable = ExtractVtable {
    base: Unknown {
        query: callback_query,
        add: callback_add,
        release: callback_release,
    },
    total,
    completed,
    stream,
    prepare,
    result,
};
static OUTPUT: OutputVtable = OutputVtable {
    base: Unknown {
        query: callback_query,
        add: callback_add,
        release: callback_release,
    },
    write,
};
#[repr(C)]
struct ArchiveVtable {
    base: Unknown,
    open: unsafe extern "C" fn(*mut c_void, *mut c_void, *const u64, *mut c_void) -> Hr,
    close: unsafe extern "C" fn(*mut c_void) -> Hr,
    count: unsafe extern "C" fn(*mut c_void, *mut u32) -> Hr,
    property: unsafe extern "C" fn(*mut c_void, u32, u32, *mut Variant) -> Hr,
    extract: unsafe extern "C" fn(*mut c_void, *const u32, u32, i32, *mut c_void) -> Hr,
}
struct Com(*mut c_void);
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            ((**self.0.cast::<*const Unknown>()).release)(self.0);
        }
    }
}
unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(module: *mut c_void, name: *const c_char) -> *mut c_void;
}
macro_rules! symbol {
    ($module:expr, $name:literal, $ty:ty) => {{
        let name = CString::new($name).unwrap();
        let pointer = dlsym($module, name.as_ptr());
        assert!(!pointer.is_null(), "missing {}", $name);
        std::mem::transmute::<*mut c_void, $ty>(pointer)
    }};
}

unsafe fn run(args: &[String]) -> i32 {
    assert_eq!(std::mem::size_of::<Variant>(), 16);
    let module = dlopen(CString::new(args[1].as_str()).unwrap().as_ptr(), 2);
    assert!(!module.is_null(), "dlopen failed");
    // Keep the module loaded until process exit, including all COM drops.
    let formats = symbol!(
        module,
        "GetNumberOfFormats",
        unsafe extern "C" fn(*mut u32) -> Hr
    );
    let prop = symbol!(
        module,
        "GetHandlerProperty2",
        unsafe extern "C" fn(u32, u32, *mut Variant) -> Hr
    );
    let create = symbol!(
        module,
        "CreateObject",
        unsafe extern "C" fn(*const Guid, *const Guid, *mut *mut c_void) -> Hr
    );
    let module_prop = symbol!(
        module,
        "GetModuleProp",
        unsafe extern "C" fn(u32, *mut Variant) -> Hr
    );
    let clear = symbol!(
        module,
        "VariantClear",
        unsafe extern "C" fn(*mut Variant) -> Hr
    );
    let length = symbol!(
        module,
        "SysStringByteLen",
        unsafe extern "C" fn(*mut u32) -> u32
    );
    let mut value = Variant::empty();
    assert_eq!(module_prop(1, &mut value), OK);
    let version = value.value.number as u32;
    assert_eq!(version, (26 << 16) | 6);
    println!(
        "version={}.{:02} wchar=4 variant=16",
        version >> 16,
        version & 0xffff
    );
    clear(&mut value);
    assert_eq!(module_prop(0, &mut value), OK);
    assert_eq!(value.value.number, 0, "unsupported IUnknown layout");
    println!("interface_type=0");
    clear(&mut value);
    let mut count = 0;
    assert_eq!(formats(&mut count), OK);
    println!("formats={count}");
    for index in 0..count {
        assert_eq!(prop(index, 1, &mut value), OK);
        assert_eq!(value.kind, 8);
        assert_eq!(length(value.value.text), 16);
        let class = ptr::read_unaligned(value.value.text.cast::<Guid>());
        clear(&mut value);
        // Dependencies are declared before archive, so archive releases first.
        let input = Com(Box::into_raw(Box::new(Input {
            vtable: &INPUT,
            refs: AtomicU32::new(1),
            file: Mutex::new(File::open(&args[2]).unwrap()),
        }))
        .cast());
        let mut archive = ptr::null_mut();
        if create(&class, &iid(6, 0x60), &mut archive) != OK {
            continue;
        }
        let archive = Com(archive);
        let table = &**archive.0.cast::<*const ArchiveVtable>();
        let offset = 0;
        if (table.open)(archive.0, input.0, &offset, ptr::null_mut()) != OK {
            (table.close)(archive.0);
            continue;
        }
        let mut items = 0;
        assert_eq!((table.count)(archive.0, &mut items), OK);
        println!("items={items}");
        for item in 0..items {
            assert_eq!((table.property)(archive.0, item, 3, &mut value), OK); // kpidPath
            if value.kind == 8 {
                let wide = std::slice::from_raw_parts(
                    value.value.text,
                    length(value.value.text) as usize / 4,
                );
                let text: String = wide
                    .iter()
                    .map(|&c| char::from_u32(c).unwrap_or('\u{fffd}'))
                    .collect();
                println!("item={item} name={text}");
            }
            clear(&mut value);
        }
        let mode = args.get(4).map(String::as_str).unwrap_or("test");
        let mut callback = Box::new(Callback {
            extract: Face {
                vtable: ptr::addr_of!(EXTRACT).cast(),
                owner: ptr::null_mut(),
            },
            output: Face {
                vtable: ptr::addr_of!(OUTPUT).cast(),
                owner: ptr::null_mut(),
            },
            refs: AtomicU32::new(1),
            cancel: mode == "cancel",
            results: AtomicU32::new(0),
            failures: AtomicU32::new(0),
            bytes: AtomicU64::new(0),
        });
        let owner = ptr::from_mut(&mut *callback);
        callback.extract.owner = owner;
        callback.output.owner = owner;
        let face = ptr::addr_of_mut!((*Box::into_raw(callback)).extract).cast();
        let callback = Com(face);
        let selected = 0;
        let test = mode == "test";
        let hr = (table.extract)(
            archive.0,
            if test { ptr::null() } else { &selected },
            if test { u32::MAX } else { 1 },
            test as i32,
            callback.0,
        );
        let state = &*owner;
        let failures = state.failures.load(Ordering::Relaxed);
        println!(
            "extract_hresult=0x{:08x} results={} failures={} bytes={}",
            hr as u32,
            state.results.load(Ordering::Relaxed),
            failures,
            state.bytes.load(Ordering::Relaxed)
        );
        (table.close)(archive.0);
        return if hr == OK && failures == 0 { 0 } else { 4 };
    }
    5
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 3 {
        std::process::exit(2);
    }
    std::process::exit(unsafe { run(&args) });
}
