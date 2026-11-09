//! Rust-owned COM interfaces. Every external entry point contains unwind boundaries.
use crate::{
    ffi::*, Api, ArchiveFlags, Control, Error, ExtraResult, ItemResult, OpenOptions, OutputFactory,
    ReadSeek, StreamError,
};
use std::{
    ffi::c_void,
    fs::File,
    io::{self, SeekFrom, Write},
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::{
        atomic::{fence, AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
};
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}
pub(crate) struct Shared {
    control: Mutex<Arc<dyn Control>>,
    password: AtomicBool,
    errors: Mutex<Vec<StreamError>>,
}
impl Shared {
    pub fn new(control: Arc<dyn Control>) -> Self {
        Self {
            control: Mutex::new(control),
            password: AtomicBool::new(false),
            errors: Mutex::new(Vec::new()),
        }
    }
    pub fn cancelled(&self) -> bool {
        let control = lock(&self.control).clone();
        control.cancelled()
    }
    fn check(&self) -> Hr {
        if self.cancelled() {
            ABORT
        } else {
            OK
        }
    }
    fn progress(&self, completed: u64, total: Option<u64>) -> Hr {
        let control = lock(&self.control).clone();
        if control.cancelled() {
            return ABORT;
        }
        control.progress(completed, total);
        OK
    }
    pub(crate) fn record(
        &self,
        operation: &str,
        index: Option<u32>,
        name: Option<String>,
        error: io::Error,
    ) {
        lock(&self.errors).push(StreamError {
            operation: operation.into(),
            index,
            name,
            kind: format!("{:?}", error.kind()),
            message: error.to_string(),
        });
    }
    pub fn errors(&self) -> Vec<StreamError> {
        lock(&self.errors).clone()
    }
    pub fn password_requested(&self) -> bool {
        self.password.load(Ordering::Relaxed)
    }
    pub fn reset(&self, control: Arc<dyn Control>) {
        *lock(&self.control) = control;
        self.password.store(false, Ordering::Relaxed);
        lock(&self.errors).clear();
    }
    pub fn error(&self, message: impl Into<String>, hresult: Hr, flags: ArchiveFlags) -> Error {
        Error {
            message: message.into(),
            hresult: Some(hresult),
            password_requested: self.password_requested(),
            flags,
            stream_errors: self.errors(),
        }
    }
}
fn guarded(shared: &Shared, call: impl FnOnce() -> Hr) -> Hr {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(hr) => hr,
        Err(_) => {
            shared.record(
                "callback",
                None,
                None,
                io::Error::other("Rust callback panicked"),
            );
            FAIL
        }
    }
}
fn ref_guard(call: impl FnOnce() -> u32) -> u32 {
    catch_unwind(AssertUnwindSafe(call)).unwrap_or(0)
}
#[repr(C)]
struct Input {
    table: *const InputVtable,
    refs: AtomicU32,
    reader: Arc<Mutex<Box<dyn ReadSeek + Send>>>,
    shared: Arc<Shared>,
}
pub(crate) fn input(reader: Arc<Mutex<Box<dyn ReadSeek + Send>>>, shared: Arc<Shared>) -> Com {
    Com(Box::into_raw(Box::new(Input {
        table: &INPUT,
        refs: AtomicU32::new(1),
        reader,
        shared,
    }))
    .cast())
}
unsafe extern "system" fn input_add(this: *mut c_void) -> u32 {
    ref_guard(|| (*this.cast::<Input>()).refs.fetch_add(1, Ordering::Relaxed) + 1)
}
unsafe extern "system" fn input_release(this: *mut c_void) -> u32 {
    ref_guard(|| {
        let p = this.cast::<Input>();
        let n = (*p).refs.fetch_sub(1, Ordering::Release) - 1;
        if n == 0 {
            fence(Ordering::Acquire);
            drop(Box::from_raw(p));
        }
        n
    })
}
unsafe extern "system" fn input_query(
    this: *mut c_void,
    id: *const Guid,
    out: *mut *mut c_void,
) -> Hr {
    let s = &*this.cast::<Input>();
    guarded(&s.shared, || {
        *out = ptr::null_mut();
        if *id == UNKNOWN || *id == iid(3, 1) || *id == iid(3, 3) {
            *out = this;
            input_add(this);
            OK
        } else {
            NO_INTERFACE
        }
    })
}
unsafe extern "system" fn input_read(
    this: *mut c_void,
    data: *mut c_void,
    size: u32,
    done: *mut u32,
) -> Hr {
    let s = &*this.cast::<Input>();
    guarded(&s.shared, || {
        if !done.is_null() {
            *done = 0;
        }
        if s.shared.check() != OK {
            return ABORT;
        }
        if size == 0 {
            return OK;
        }
        let mut reader = lock(&s.reader);
        match reader.read(std::slice::from_raw_parts_mut(data.cast(), size as usize)) {
            Ok(n) => {
                if !done.is_null() {
                    *done = n as u32;
                }
                OK
            }
            Err(e) => {
                s.shared.record("read", None, None, e);
                FAIL
            }
        }
    })
}
unsafe extern "system" fn input_seek(
    this: *mut c_void,
    offset: i64,
    origin: u32,
    position: *mut u64,
) -> Hr {
    let s = &*this.cast::<Input>();
    guarded(&s.shared, || {
        if s.shared.check() != OK {
            return ABORT;
        }
        let from = match origin {
            0 if offset >= 0 => SeekFrom::Start(offset as u64),
            1 => SeekFrom::Current(offset),
            2 => SeekFrom::End(offset),
            _ => return FAIL,
        };
        match lock(&s.reader).seek(from) {
            Ok(n) => {
                if !position.is_null() {
                    *position = n;
                }
                OK
            }
            Err(e) => {
                s.shared.record("seek", None, None, e);
                FAIL
            }
        }
    })
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

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;
#[repr(C)]
struct Output {
    table: *const OutputVtable,
    refs: AtomicU32,
    writer: Writer,
    shared: Arc<Shared>,
    index: u32,
}
fn output(writer: Writer, shared: Arc<Shared>, index: u32) -> *mut c_void {
    Box::into_raw(Box::new(Output {
        table: &OUTPUT,
        refs: AtomicU32::new(1),
        writer,
        shared,
        index,
    }))
    .cast()
}
unsafe extern "system" fn output_add(this: *mut c_void) -> u32 {
    ref_guard(|| {
        (*this.cast::<Output>())
            .refs
            .fetch_add(1, Ordering::Relaxed)
            + 1
    })
}
unsafe extern "system" fn output_release(this: *mut c_void) -> u32 {
    ref_guard(|| {
        let p = this.cast::<Output>();
        let n = (*p).refs.fetch_sub(1, Ordering::Release) - 1;
        if n == 0 {
            fence(Ordering::Acquire);
            drop(Box::from_raw(p));
        }
        n
    })
}
unsafe extern "system" fn output_query(
    this: *mut c_void,
    id: *const Guid,
    out: *mut *mut c_void,
) -> Hr {
    let s = &*this.cast::<Output>();
    guarded(&s.shared, || {
        *out = ptr::null_mut();
        if *id == UNKNOWN || *id == iid(3, 2) {
            *out = this;
            output_add(this);
            OK
        } else {
            NO_INTERFACE
        }
    })
}
unsafe extern "system" fn output_write(
    this: *mut c_void,
    data: *const c_void,
    size: u32,
    done: *mut u32,
) -> Hr {
    let s = &*this.cast::<Output>();
    guarded(&s.shared, || {
        if !done.is_null() {
            *done = 0;
        }
        if s.shared.check() != OK {
            return ABORT;
        }
        if size == 0 {
            return OK;
        }
        let bytes = std::slice::from_raw_parts(data.cast::<u8>(), size as usize);
        match lock(&s.writer).write(bytes) {
            Ok(n) => {
                if !done.is_null() {
                    *done = n as u32;
                }
                if n == 0 {
                    s.shared.record(
                        "write",
                        Some(s.index),
                        None,
                        io::ErrorKind::WriteZero.into(),
                    );
                    FAIL
                } else {
                    OK
                }
            }
            Err(e) => {
                s.shared.record("write", Some(s.index), None, e);
                FAIL
            }
        }
    })
}
static OUTPUT: OutputVtable = OutputVtable {
    base: Unknown {
        query: output_query,
        add: output_add,
        release: output_release,
    },
    write: output_write,
};

#[repr(C)]
struct Face {
    table: *const c_void,
    owner: *mut Callback,
}
struct ExtractState {
    current: Option<u32>,
    writer: Option<Writer>,
    items: Vec<ItemResult>,
    extra: Vec<ExtraResult>,
}
pub(crate) struct Callback {
    open: Face,
    extract: Face,
    volume: Face,
    password: Face,
    message: Face,
    refs: AtomicU32,
    api: Arc<Api>,
    options: OpenOptions,
    shared: Arc<Shared>,
    factory: Option<Arc<dyn OutputFactory>>,
    state: Mutex<ExtractState>,
    total: AtomicU64,
    has_total: AtomicBool,
}
impl Callback {
    pub fn new(
        api: Arc<Api>,
        options: OpenOptions,
        shared: Arc<Shared>,
        factory: Option<Arc<dyn OutputFactory>>,
    ) -> (Com, *mut Self) {
        let face = || Face {
            table: ptr::null(),
            owner: ptr::null_mut(),
        };
        let mut callback = Box::new(Self {
            open: face(),
            extract: face(),
            volume: face(),
            password: face(),
            message: face(),
            refs: AtomicU32::new(1),
            api,
            options,
            shared,
            factory,
            state: Mutex::new(ExtractState {
                current: None,
                writer: None,
                items: Vec::new(),
                extra: Vec::new(),
            }),
            total: AtomicU64::new(0),
            has_total: AtomicBool::new(false),
        });
        let p = ptr::from_mut(&mut *callback);
        callback.open = Face {
            table: ptr::addr_of!(OPEN).cast(),
            owner: p,
        };
        callback.extract = Face {
            table: ptr::addr_of!(EXTRACT).cast(),
            owner: p,
        };
        callback.volume = Face {
            table: ptr::addr_of!(VOLUME).cast(),
            owner: p,
        };
        callback.password = Face {
            table: ptr::addr_of!(PASSWORD).cast(),
            owner: p,
        };
        callback.message = Face {
            table: ptr::addr_of!(MESSAGE).cast(),
            owner: p,
        };
        let extracting = callback.factory.is_some();
        let p = Box::into_raw(callback);
        let face = unsafe {
            if extracting {
                ptr::addr_of_mut!((*p).extract)
            } else {
                ptr::addr_of_mut!((*p).open)
            }
        };
        (Com(face.cast()), p)
    }
    pub unsafe fn results(this: *mut Self) -> (Vec<ItemResult>, Vec<ExtraResult>) {
        let mut state = lock(&(*this).state);
        (
            std::mem::take(&mut state.items),
            std::mem::take(&mut state.extra),
        )
    }
}
unsafe fn owner(this: *mut c_void) -> *mut Callback {
    (*this.cast::<Face>()).owner
}
unsafe extern "system" fn callback_add(this: *mut c_void) -> u32 {
    ref_guard(|| (*owner(this)).refs.fetch_add(1, Ordering::Relaxed) + 1)
}
unsafe extern "system" fn callback_release(this: *mut c_void) -> u32 {
    ref_guard(|| {
        let p = owner(this);
        let n = (*p).refs.fetch_sub(1, Ordering::Release) - 1;
        if n == 0 {
            fence(Ordering::Acquire);
            drop(Box::from_raw(p));
        }
        n
    })
}
unsafe extern "system" fn callback_query(
    this: *mut c_void,
    id: *const Guid,
    out: *mut *mut c_void,
) -> Hr {
    let p = owner(this);
    let s = &*p;
    guarded(&s.shared, || {
        *out = ptr::null_mut();
        let face = if *id == UNKNOWN {
            if s.factory.is_some() {
                ptr::addr_of_mut!((*p).extract)
            } else {
                ptr::addr_of_mut!((*p).open)
            }
        } else if *id == iid(6, 0x10) {
            ptr::addr_of_mut!((*p).open)
        } else if *id == iid(6, 0x20) || *id == iid(0, 5) {
            ptr::addr_of_mut!((*p).extract)
        } else if *id == iid(6, 0x30) {
            ptr::addr_of_mut!((*p).volume)
        } else if *id == iid(5, 0x10) {
            ptr::addr_of_mut!((*p).password)
        } else if *id == iid(6, 0x22) {
            ptr::addr_of_mut!((*p).message)
        } else {
            return NO_INTERFACE;
        };
        *out = face.cast();
        callback_add(*out);
        OK
    })
}
const fn base() -> Unknown {
    Unknown {
        query: callback_query,
        add: callback_add,
        release: callback_release,
    }
}
unsafe extern "system" fn open_total(
    this: *mut c_void,
    _files: *const u64,
    bytes: *const u64,
) -> Hr {
    let s = &*owner(this);
    guarded(&s.shared, || {
        if !bytes.is_null() {
            s.total.store(*bytes, Ordering::Relaxed);
            s.has_total.store(true, Ordering::Relaxed);
        }
        s.shared.check()
    })
}
unsafe extern "system" fn open_completed(
    this: *mut c_void,
    _files: *const u64,
    bytes: *const u64,
) -> Hr {
    let s = &*owner(this);
    guarded(&s.shared, || {
        if !bytes.is_null() {
            s.shared.progress(
                *bytes,
                s.has_total
                    .load(Ordering::Relaxed)
                    .then(|| s.total.load(Ordering::Relaxed)),
            )
        } else {
            s.shared.check()
        }
    })
}
unsafe extern "system" fn extract_total(this: *mut c_void, total: u64) -> Hr {
    let s = &*owner(this);
    guarded(&s.shared, || {
        s.total.store(total, Ordering::Relaxed);
        s.has_total.store(true, Ordering::Relaxed);
        s.shared.check()
    })
}
unsafe extern "system" fn extract_completed(this: *mut c_void, bytes: *const u64) -> Hr {
    open_completed(this, ptr::null(), bytes)
}
unsafe extern "system" fn volume_property(this: *mut c_void, id: u32, value: *mut Variant) -> Hr {
    let s = &*owner(this);
    guarded(&s.shared, || {
        *value = Variant::empty();
        if s.shared.check() != OK {
            return ABORT;
        }
        match id {
            4 => match s.api.string(&s.options.name) {
                Ok(p) => {
                    (*value).kind = 8;
                    (*value).value.text = p;
                }
                Err(hr) => return hr,
            },
            6 => {
                (*value).kind = 11;
                (*value).value.number = 0;
            }
            _ => {}
        }
        OK
    })
}
unsafe extern "system" fn volume_stream(
    this: *mut c_void,
    name: *const Wide,
    out: *mut *mut c_void,
) -> Hr {
    let s = &*owner(this);
    guarded(&s.shared, || {
        *out = ptr::null_mut();
        if s.shared.check() != OK {
            return ABORT;
        }
        let name = nul_text(name);
        let Some(path) = s.options.volumes.get(&name) else {
            s.shared.record(
                "volume",
                None,
                Some(name),
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Archive requested an unmapped volume",
                ),
            );
            return FALSE;
        };
        match File::open(path) {
            Ok(file) => {
                let stream = input(Arc::new(Mutex::new(Box::new(file))), s.shared.clone());
                *out = stream.0;
                std::mem::forget(stream);
                OK
            }
            Err(e) => {
                s.shared.record("volume", None, Some(name), e);
                FAIL
            }
        }
    })
}
unsafe extern "system" fn password(this: *mut c_void, out: *mut *mut Wide) -> Hr {
    let s = &*owner(this);
    guarded(&s.shared, || {
        *out = ptr::null_mut();
        s.shared.password.store(true, Ordering::Relaxed);
        if s.shared.check() != OK {
            return ABORT;
        }
        let Some(text) = &s.options.password else {
            return ABORT;
        };
        match s.api.password(text) {
            Ok(p) => {
                *out = p;
                OK
            }
            Err(hr) => hr,
        }
    })
}
unsafe extern "system" fn extract_stream(
    this: *mut c_void,
    index: u32,
    out: *mut *mut c_void,
    mode: i32,
) -> Hr {
    let s = &*owner(this);
    guarded(&s.shared, || {
        *out = ptr::null_mut();
        {
            let mut state = lock(&s.state);
            state.current = Some(index);
            state.writer = None;
        }
        if s.shared.check() != OK {
            return ABORT;
        }
        if mode != 0 {
            return OK;
        }
        let Some(factory) = &s.factory else {
            return FAIL;
        };
        match factory.open(index) {
            Ok(Some(writer)) => {
                let writer = Arc::new(Mutex::new(writer));
                lock(&s.state).writer = Some(writer.clone());
                *out = output(writer, s.shared.clone(), index);
                OK
            }
            Ok(None) => OK,
            Err(e) => {
                s.shared.record("open_output", Some(index), None, e);
                FAIL
            }
        }
    })
}
unsafe extern "system" fn prepare(this: *mut c_void, _mode: i32) -> Hr {
    let s = &*owner(this);
    guarded(&s.shared, || s.shared.check())
}
unsafe extern "system" fn result(this: *mut c_void, result: i32) -> Hr {
    let s = &*owner(this);
    guarded(&s.shared, || {
        let (index, writer) = {
            let mut state = lock(&s.state);
            (state.current.take(), state.writer.take())
        };
        let mut hr = OK;
        if let Some(writer) = writer {
            if let Err(e) = lock(&writer).flush() {
                s.shared.record("flush", index, None, e);
                hr = FAIL;
            }
        }
        if let Some(index) = index {
            lock(&s.state).items.push(ItemResult { index, result });
        }
        if hr != OK {
            hr
        } else {
            s.shared.check()
        }
    })
}
unsafe extern "system" fn message(
    this: *mut c_void,
    index_type: u32,
    index: u32,
    result: i32,
) -> Hr {
    let s = &*owner(this);
    guarded(&s.shared, || {
        lock(&s.state).extra.push(ExtraResult {
            index_type,
            index,
            result,
        });
        s.shared.check()
    })
}
static OPEN: OpenVtable = OpenVtable {
    base: base(),
    total: open_total,
    completed: open_completed,
};
static VOLUME: VolumeVtable = VolumeVtable {
    base: base(),
    property: volume_property,
    stream: volume_stream,
};
static PASSWORD: PasswordVtable = PasswordVtable {
    base: base(),
    password,
};
static MESSAGE: MessageVtable = MessageVtable {
    base: base(),
    result: message,
};
static EXTRACT: ExtractVtable = ExtractVtable {
    base: base(),
    total: extract_total,
    completed: extract_completed,
    stream: extract_stream,
    prepare,
    result,
};
