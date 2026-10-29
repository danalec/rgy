// SPDX-License-Identifier: Apache-2.0

//! Whole-disk FSEvents stream, directory granularity, replayable by event id.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char, c_int, c_void};

use rgy_par::mpsc::Sender;
use rgy_par::sync::{Mutex, OnceCell};

use crate::event::Event;

/// `kFSEventStreamCreateFlagNoDefer` (0x2): the framework delivers an event
/// as soon as it happens instead of waiting for the latency window to
/// close. It is *not* `kFSEventStreamCreateFlagFileEvents` (0x10), so the
/// stream stays directory-granular: it names the folder whose contents
/// changed, not the item that changed.
const CREATE_FLAG_NO_DEFER: u32 = 0x2;

// `RTLD_LAZY`: the framework's symbols are resolved on first use, not on
// load. `dlfcn.h`.
const RTLD_LAZY: c_int = 0x1;

// `QOS_CLASS_USER_INITIATED`: the queue the stream's callback runs on.
// `sys/qos.h`.
const QOS_CLASS_USER_INITIATED: u32 = 0x19;

/// A stream's callback context.
///
/// The callback runs on a GCD queue — a thread the Rust runtime does not
/// own — so the context is shared across threads and has to be `Sync`. The
/// sender is behind a lock, which is what makes it so: `std`'s channel
/// sender is `Send` but not `Sync`, and the tree's own one is both.
struct Info {
    tx: Mutex<Sender<Vec<Event>>>,
    stream: u32,
}

#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

type Callback = extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);

/// CoreServices (and the CoreFoundation it brings) are loaded on first use,
/// not linked: CoreFoundation's initializer costs every process that links
/// it ~1 ms at launch, and only the daemon watches the disk.
struct Api {
    create: unsafe extern "C" fn(
        *const c_void,
        Callback,
        *const Context,
        *const c_void,
        u64,
        f64,
        u32,
    ) -> *mut c_void,
    set_queue: unsafe extern "C" fn(*mut c_void, *mut c_void),
    start: unsafe extern "C" fn(*mut c_void) -> u8,
    stop: unsafe extern "C" fn(*mut c_void),
    invalidate: unsafe extern "C" fn(*mut c_void),
    release: unsafe extern "C" fn(*mut c_void),
    current_id: unsafe extern "C" fn() -> u64,
    string: unsafe extern "C" fn(*const c_void, *const i8, u32) -> *const c_void,
    array: unsafe extern "C" fn(
        *const c_void,
        *const *const c_void,
        isize,
        *const c_void,
    ) -> *const c_void,
    array_callbacks: *const c_void,
}

// Function pointers and a constant's address: fine to share.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> &'static Api {
    static API: OnceCell<Api> = OnceCell::new();
    API.get_or_init(|| unsafe {
        let open = |path: &CStr| {
            let h = dlopen(path.as_ptr(), RTLD_LAZY);
            assert!(!h.is_null(), "cannot load {path:?}");
            h
        };
        let (cs, cf) = (
            open(c"/System/Library/Frameworks/CoreServices.framework/CoreServices"),
            open(c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"),
        );
        Api {
            create: sym(cs, c"FSEventStreamCreate"),
            set_queue: sym(cs, c"FSEventStreamSetDispatchQueue"),
            start: sym(cs, c"FSEventStreamStart"),
            stop: sym(cs, c"FSEventStreamStop"),
            invalidate: sym(cs, c"FSEventStreamInvalidate"),
            release: sym(cs, c"FSEventStreamRelease"),
            current_id: sym(cs, c"FSEventsGetCurrentEventId"),
            string: sym(cf, c"CFStringCreateWithCString"),
            array: sym(cf, c"CFArrayCreate"),
            array_callbacks: sym(cf, c"kCFTypeArrayCallBacks"),
        }
    })
}

/// A symbol of a loaded image, as `T` (a function pointer, or a data
/// symbol's address).
unsafe fn sym<T>(h: *mut c_void, name: &CStr) -> T {
    let p = unsafe { dlsym(h, name.as_ptr()) };
    assert!(!p.is_null(), "missing {name:?}");
    unsafe { core::mem::transmute_copy(&p) }
}

/// The newest event id on the system.
pub fn current_id() -> u64 {
    unsafe { (api().current_id)() }
}

unsafe extern "C" {
    // The dynamic loader: the frameworks are loaded on first use, not
    // linked. `dlfcn.h`.
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dispatch_queue_create(label: *const i8, attr: *const c_void) -> *mut c_void;
    fn dispatch_queue_attr_make_with_qos_class(
        attr: *const c_void,
        qos: u32,
        relative_priority: i32,
    ) -> *const c_void;
}

extern "C" fn on_events(
    _s: *mut c_void,
    info: *mut c_void,
    n: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    let info = unsafe { &*(info as *const Info) };
    let paths = paths as *const *const i8;
    let batch = (0..n)
        .map(|i| unsafe {
            Event {
                path: CStr::from_ptr(*paths.add(i)).to_bytes().to_vec(),
                flags: *flags.add(i),
                id: *ids.add(i),
                stream: info.stream,
            }
        })
        .collect();
    let _ = info.tx.lock().unwrap().send(batch);
}

/// A running stream; dropping it stops it.
pub struct Stream(*mut c_void);

// The stream is only started and stopped, never shared mid-call.
unsafe impl Send for Stream {}
unsafe impl Sync for Stream {}

/// The stream is what the feed keeps alive: dropping it stops the feed,
/// and a stopped feed is a daemon that stopped noticing changes.
impl crate::feed::KeepAlive for Stream {}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            let a = api();
            (a.stop)(self.0);
            (a.invalidate)(self.0);
            (a.release)(self.0);
        }
    }
}

/// Watch `/` from `since` (an event id). Batches of directory-level events,
/// stamped with `stream`, arrive on `tx` until the returned stream is dropped.
pub fn watch(since: u64, latency: f64, tx: Sender<Vec<Event>>, stream: u32) -> Stream {
    let a = api();
    unsafe {
        let root = (a.string)(core::ptr::null(), c"/".as_ptr(), 0x0800_0100);
        let arr = (a.array)(core::ptr::null(), &root, 1, a.array_callbacks);
        // The sender is leaked: a callback may still be in flight when the
        // stream stops, and streams are replaced rarely.
        let ctx = Context {
            version: 0,
            info: Box::into_raw(Box::new(Info {
                tx: Mutex::new(tx),
                stream,
            })) as *mut c_void,
            retain: core::ptr::null(),
            release: core::ptr::null(),
            copy_description: core::ptr::null(),
        };
        // Not IgnoreSelf: linked into an app, the app's own renames and moves
        // are exactly what its search must see.
        let s = (a.create)(
            core::ptr::null(),
            on_events,
            &ctx,
            arr,
            since,
            latency,
            CREATE_FLAG_NO_DEFER,
        );
        // The callback only copies paths out, but it must not queue behind
        // busy threads (a first build runs on every core at user-initiated
        // QoS): fseventsd drops what a client falls behind on, and the drop
        // costs a relist of every folder changed since the last sync.
        let qos = QOS_CLASS_USER_INITIATED;
        let q = dispatch_queue_create(
            c"fsearch.fsevents".as_ptr(),
            dispatch_queue_attr_make_with_qos_class(core::ptr::null(), qos, 0),
        );
        (a.set_queue)(s, q);
        (a.start)(s);
        Stream(s)
    }
}
