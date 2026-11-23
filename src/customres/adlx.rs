//! AMD custom resolutions and GPU scaling through ADLX (`amdadlx64.dll` of
//! the driver).
//!
//! ADLX is a C interface with vtables. The first field of every object is the
//! vtable pointer. The slot numbers come from the public ADLX SDK headers and
//! were tested on an RX 9070 XT. Every interface that ADLX gives out must be
//! released, otherwise `ADLXTerminate` reports orphan objects.

use std::ffi::c_void;
use std::fmt;
use std::ptr::{NonNull, null_mut};

use anyhow::{Context, bail};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::{PCSTR, s, w};

use crate::display::{self, DesktopRect};

mod customres;
mod scaling;

pub use customres::clean_up_pending;

/// The header version 2.0.0.125 that the binding follows.
const ADLX_FULL_VERSION: u64 = (2 << 48) | 125;

const ADLX_OK: i32 = 0;
const ADLX_ALREADY_ENABLED: i32 = 1;
const ADLX_ALREADY_INITIALIZED: i32 = 2;
const ADLX_FAIL: i32 = 3;
const ADLX_NOT_SUPPORTED: i32 = 12;

const RESULT_NAMES: [&str; 19] = [
    "ADLX_OK",
    "ADLX_ALREADY_ENABLED",
    "ADLX_ALREADY_INITIALIZED",
    "ADLX_FAIL",
    "ADLX_INVALID_ARGS",
    "ADLX_BAD_VER",
    "ADLX_UNKNOWN_INTERFACE",
    "ADLX_TERMINATED",
    "ADLX_ADL_INIT_ERROR",
    "ADLX_NOT_FOUND",
    "ADLX_INVALID_OBJECT",
    "ADLX_ORPHAN_OBJECTS",
    "ADLX_NOT_SUPPORTED",
    "ADLX_PENDING_OPERATION",
    "ADLX_GPU_INACTIVE",
    "ADLX_GPU_IN_USE",
    "ADLX_TIMEOUT_OPERATION",
    "ADLX_NOT_ACTIVE",
    "ADLX_RESET_NEEDED",
];

fn result_name(code: i32) -> String {
    usize::try_from(code)
        .ok()
        .and_then(|i| RESULT_NAMES.get(i))
        .map_or_else(|| format!("ADLX result {code}"), |name| (*name).to_owned())
}

// IADLXInterface
const RELEASE: usize = 1;
// IADLXSystem. It has no IADLXInterface prefix and no Release.
const SYS_GET_DISPLAYS_SERVICES: usize = 3;
const SYS_GET_DESKTOPS_SERVICES: usize = 4;
// IADLXList and the typed lists
const LIST_SIZE: usize = 3;
const LIST_AT: usize = 11;
// IADLXDisplayServices
const DS_GET_DISPLAYS: usize = 4;
// IADLXDisplay
const DISPLAY_NATIVE_RESOLUTION: usize = 8;
const DISPLAY_UNIQUE_ID: usize = 13;
// IADLXDesktopServices
const DKS_GET_DESKTOPS: usize = 4;
// IADLXDesktop
const DESKTOP_SIZE: usize = 4;
const DESKTOP_TOP_LEFT: usize = 5;
const DESKTOP_GET_DISPLAYS: usize = 8;

type FnQueryFullVersion = unsafe extern "C" fn(*mut u64) -> i32;
type FnInitialize = unsafe extern "C" fn(u64, *mut *mut c_void) -> i32;
type FnInitialize2 = unsafe extern "C" fn(u64, *mut *mut c_void, *mut *mut c_void) -> i32;
type FnTerminate = unsafe extern "C" fn() -> i32;

type FnSelf = unsafe extern "C" fn(*mut c_void) -> i32;
type FnSize = unsafe extern "C" fn(*mut c_void) -> u32;
type FnOutPtr = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32;
type FnArgOutPtr = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32;
type FnAt = unsafe extern "C" fn(*mut c_void, u32, *mut *mut c_void) -> i32;
type FnOutBool = unsafe extern "C" fn(*mut c_void, *mut u8) -> i32;
type FnInBool = unsafe extern "C" fn(*mut c_void, u8) -> i32;
type FnOutI32 = unsafe extern "C" fn(*mut c_void, *mut i32) -> i32;
type FnInI32 = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type FnOut2I32 = unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> i32;
type FnOutUsize = unsafe extern "C" fn(*mut c_void, *mut usize) -> i32;

/// A failed ADLX call.
#[derive(Debug)]
struct AdlxError {
    what: &'static str,
    code: i32,
}

impl fmt::Display for AdlxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed: {}", self.what, result_name(self.code))
    }
}

impl std::error::Error for AdlxError {}

impl AdlxError {
    fn is_not_supported(&self) -> bool {
        self.code == ADLX_NOT_SUPPORTED
    }
}

fn check(code: i32, what: &'static str) -> Result<(), AdlxError> {
    if code == ADLX_OK {
        Ok(())
    } else {
        Err(AdlxError { what, code })
    }
}

/// Reads function `index` from the vtable of `object`.
///
/// # Safety
///
/// `object` must be a live ADLX object, `index` a slot of its vtable and `F`
/// the function type of that slot.
unsafe fn slot<F: Copy>(object: NonNull<c_void>, index: usize) -> F {
    debug_assert_eq!(size_of::<F>(), size_of::<*const c_void>());
    // SAFETY: the caller guarantees the object and the slot. The first field
    // of the object is the vtable pointer.
    unsafe {
        let vtable = *object.as_ptr().cast::<*const *const c_void>();
        std::mem::transmute_copy::<*const c_void, F>(&*vtable.add(index))
    }
}

/// An ADLX interface pointer. Drop releases it.
struct Iface(NonNull<c_void>);

impl Iface {
    fn new(ptr: *mut c_void, what: &'static str) -> Result<Self, AdlxError> {
        NonNull::new(ptr).map(Self).ok_or(AdlxError {
            what,
            code: ADLX_FAIL,
        })
    }

    fn ptr(&self) -> *mut c_void {
        self.0.as_ptr()
    }

    /// # Safety
    ///
    /// `index` must be a slot of this interface with the function type `F`.
    unsafe fn slot<F: Copy>(&self, index: usize) -> F {
        // SAFETY: the interface is live while `self` exists.
        unsafe { slot(self.0, index) }
    }

    /// Calls a getter that returns an interface.
    ///
    /// # Safety
    ///
    /// `index` must be a slot of the type `FnOutPtr`.
    unsafe fn get(&self, index: usize, what: &'static str) -> Result<Iface, AdlxError> {
        let mut out = null_mut();
        // SAFETY: the caller guarantees the slot type.
        check(
            unsafe { self.slot::<FnOutPtr>(index)(self.ptr(), &mut out) },
            what,
        )?;
        Iface::new(out, what)
    }

    /// Calls a getter that takes an interface and returns one.
    ///
    /// # Safety
    ///
    /// `index` must be a slot of the type `FnArgOutPtr`.
    unsafe fn get_for(
        &self,
        index: usize,
        arg: &Iface,
        what: &'static str,
    ) -> Result<Iface, AdlxError> {
        let mut out = null_mut();
        // SAFETY: the caller guarantees the slot type. `arg` is live.
        let code = unsafe { self.slot::<FnArgOutPtr>(index)(self.ptr(), arg.ptr(), &mut out) };
        check(code, what)?;
        Iface::new(out, what)
    }

    /// # Safety
    ///
    /// `index` must be a slot of the type `FnOutBool`.
    unsafe fn get_bool(&self, index: usize, what: &'static str) -> Result<bool, AdlxError> {
        let mut value = 0u8;
        // SAFETY: the caller guarantees the slot type.
        check(
            unsafe { self.slot::<FnOutBool>(index)(self.ptr(), &mut value) },
            what,
        )?;
        Ok(value != 0)
    }

    /// # Safety
    ///
    /// `index` must be a slot of the type `FnInBool`.
    unsafe fn set_bool(
        &self,
        index: usize,
        value: bool,
        what: &'static str,
    ) -> Result<(), AdlxError> {
        // SAFETY: the caller guarantees the slot type.
        check(
            unsafe { self.slot::<FnInBool>(index)(self.ptr(), u8::from(value)) },
            what,
        )
    }

    /// # Safety
    ///
    /// `index` must be a slot of the type `FnOutI32`.
    unsafe fn get_i32(&self, index: usize, what: &'static str) -> Result<i32, AdlxError> {
        let mut value = 0;
        // SAFETY: the caller guarantees the slot type.
        check(
            unsafe { self.slot::<FnOutI32>(index)(self.ptr(), &mut value) },
            what,
        )?;
        Ok(value)
    }

    /// # Safety
    ///
    /// `index` must be a slot of the type `FnInI32`.
    unsafe fn set_i32(
        &self,
        index: usize,
        value: i32,
        what: &'static str,
    ) -> Result<(), AdlxError> {
        // SAFETY: the caller guarantees the slot type.
        check(
            unsafe { self.slot::<FnInI32>(index)(self.ptr(), value) },
            what,
        )
    }

    /// # Safety
    ///
    /// `index` must be a slot of the type `FnOut2I32`.
    unsafe fn get_2_i32(&self, index: usize, what: &'static str) -> Result<(i32, i32), AdlxError> {
        let (mut a, mut b) = (0, 0);
        // SAFETY: the caller guarantees the slot type.
        check(
            unsafe { self.slot::<FnOut2I32>(index)(self.ptr(), &mut a, &mut b) },
            what,
        )?;
        Ok((a, b))
    }

    /// The items of a typed ADLX list.
    fn items(&self, what: &'static str) -> Result<Vec<Iface>, AdlxError> {
        // SAFETY: `self` is a list. Size and the typed At are list slots.
        let size = unsafe { self.slot::<FnSize>(LIST_SIZE)(self.ptr()) };
        (0..size)
            .map(|i| {
                let mut out = null_mut();
                // SAFETY: `i` is below the list size.
                check(
                    unsafe { self.slot::<FnAt>(LIST_AT)(self.ptr(), i, &mut out) },
                    what,
                )?;
                Iface::new(out, what)
            })
            .collect()
    }
}

impl Drop for Iface {
    fn drop(&mut self) {
        // SAFETY: every ADLX interface starts with IADLXInterface.
        let _ = unsafe { self.slot::<FnSelf>(RELEASE)(self.ptr()) };
    }
}

/// An initialized ADLX. Drop calls `ADLXTerminate`. The library stays
/// loaded: ADLX can keep worker threads until the process ends.
struct Session {
    terminate: FnTerminate,
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: balances a successful ADLXInitialize.
        let code = unsafe { (self.terminate)() };
        if code != ADLX_OK {
            warn!("ADLXTerminate returned {}", result_name(code));
        }
    }
}

/// ADLX for one display.
pub struct Amd {
    // Fields drop in this order: the interfaces before the session.
    display: Iface,
    display_services: Iface,
    _session: Session,
    display_name: String,
}

impl fmt::Debug for Amd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Amd")
            .field("display_name", &self.display_name)
            .finish_non_exhaustive()
    }
}

/// Reads an export of `amdadlx64.dll`.
fn export<F: Copy>(module: windows::Win32::Foundation::HMODULE, name: PCSTR) -> Option<F> {
    // SAFETY: plain export lookup. The caller names the right type.
    unsafe { GetProcAddress(module, name) }.map(|f| {
        // SAFETY: `F` is the documented signature of the export.
        unsafe { std::mem::transmute_copy::<unsafe extern "system" fn() -> isize, F>(&f) }
    })
}

impl Amd {
    /// Loads ADLX and finds the display. `None` means no AMD driver or the
    /// display is not on an AMD GPU.
    pub fn detect(display_name: &str) -> Option<Self> {
        match Self::load(display_name) {
            Ok(amd) => Some(amd),
            Err(err) => {
                info!("no AMD ADLX support: {err:#}");
                None
            }
        }
    }

    fn load(display_name: &str) -> anyhow::Result<Self> {
        // Only System32, so a planted DLL next to the exe cannot load.
        // SAFETY: plain library load from the system folder.
        let module =
            unsafe { LoadLibraryExW(w!("amdadlx64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .context("amdadlx64.dll is not installed")?;
        if let Some(query) = export::<FnQueryFullVersion>(module, s!("ADLXQueryFullVersion")) {
            let mut version = 0u64;
            // SAFETY: valid output pointer.
            if unsafe { query(&mut version) } == ADLX_OK {
                info!("ADLX version {}", version_text(version));
            }
        }
        let terminate = export::<FnTerminate>(module, s!("ADLXTerminate"))
            .context("amdadlx64.dll has no ADLXTerminate")?;
        let mut system = null_mut();
        let code = if let Some(init2) = export::<FnInitialize2>(module, s!("ADLXInitialize2")) {
            let mut mapping = null_mut();
            // SAFETY: valid output pointers.
            unsafe { init2(ADLX_FULL_VERSION, &mut system, &mut mapping) }
        } else {
            let init = export::<FnInitialize>(module, s!("ADLXInitialize"))
                .context("amdadlx64.dll has no ADLXInitialize")?;
            // SAFETY: valid output pointer.
            unsafe { init(ADLX_FULL_VERSION, &mut system) }
        };
        if !matches!(
            code,
            ADLX_OK | ADLX_ALREADY_ENABLED | ADLX_ALREADY_INITIALIZED
        ) {
            bail!("ADLXInitialize failed: {}", result_name(code));
        }
        let session = Session { terminate };
        let system = NonNull::new(system).context("ADLXInitialize gave no system")?;

        let display_services =
            system_get(system, SYS_GET_DISPLAYS_SERVICES, "GetDisplaysServices")?;
        let desktop_services =
            system_get(system, SYS_GET_DESKTOPS_SERVICES, "GetDesktopsServices")?;
        let wanted = display::desktop_rect(display_name)?;
        let unique_id = find_unique_id(&desktop_services, wanted)?;
        drop(desktop_services);
        let display = find_display(&display_services, unique_id)?;
        // SAFETY: NativeResolution has two int outputs.
        let (width, height) =
            unsafe { display.get_2_i32(DISPLAY_NATIVE_RESOLUTION, "NativeResolution") }?;
        let offers_native = display::list_modes(display_name).iter().any(|m| {
            i64::from(m.width) == i64::from(width) && i64::from(m.height) == i64::from(height)
        });
        if !offers_native {
            bail!(
                "the ADLX display has the native resolution {width}x{height}, {display_name} does not offer it"
            );
        }
        Ok(Self {
            display,
            display_services,
            _session: session,
            display_name: display_name.to_owned(),
        })
    }
}

/// Calls an `IADLXSystem` getter. The system object is not an
/// IADLXInterface, so it has its own slot numbers.
fn system_get(
    system: NonNull<c_void>,
    index: usize,
    what: &'static str,
) -> Result<Iface, AdlxError> {
    let mut out = null_mut();
    // SAFETY: `index` is a getter slot of IADLXSystem.
    let code = unsafe { slot::<FnOutPtr>(system, index)(system.as_ptr(), &mut out) };
    check(code, what)?;
    Iface::new(out, what)
}

/// An ADLX desktop with the unique IDs of its displays.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DesktopEntry {
    rect: DesktopRect,
    display_ids: Vec<usize>,
}

fn find_unique_id(desktop_services: &Iface, wanted: DesktopRect) -> anyhow::Result<usize> {
    // SAFETY: GetDesktops has an interface output.
    let list = unsafe { desktop_services.get(DKS_GET_DESKTOPS, "GetDesktops") }?;
    let mut entries = Vec::new();
    for desktop in list.items("GetDesktops")? {
        // SAFETY: Size and TopLeft have two int outputs (ADLX_Point is two ints).
        let (width, height) = unsafe { desktop.get_2_i32(DESKTOP_SIZE, "Desktop Size") }?;
        // SAFETY: as above.
        let (x, y) = unsafe { desktop.get_2_i32(DESKTOP_TOP_LEFT, "Desktop TopLeft") }?;
        // SAFETY: GetDisplays has an interface output.
        let displays = unsafe { desktop.get(DESKTOP_GET_DISPLAYS, "Desktop GetDisplays") }?;
        let display_ids = displays
            .items("Desktop GetDisplays")?
            .iter()
            .map(unique_id)
            .collect::<Result<Vec<_>, _>>()?;
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            continue;
        };
        entries.push(DesktopEntry {
            rect: DesktopRect {
                x,
                y,
                width,
                height,
            },
            display_ids,
        });
    }
    match_desktop(&entries, wanted)
}

fn unique_id(display: &Iface) -> Result<usize, AdlxError> {
    let mut id = 0usize;
    // SAFETY: UniqueId has a size_t output.
    let code = unsafe { display.slot::<FnOutUsize>(DISPLAY_UNIQUE_ID)(display.ptr(), &mut id) };
    check(code, "UniqueId")?;
    Ok(id)
}

/// The unique ID of the only display on the desktop at `wanted`.
fn match_desktop(entries: &[DesktopEntry], wanted: DesktopRect) -> anyhow::Result<usize> {
    let matches: Vec<&DesktopEntry> = entries.iter().filter(|e| e.rect == wanted).collect();
    let [entry] = matches.as_slice() else {
        bail!(
            "{} ADLX desktops are at {},{} with {}x{}, expected one",
            matches.len(),
            wanted.x,
            wanted.y,
            wanted.width,
            wanted.height
        );
    };
    match entry.display_ids.as_slice() {
        [id] => Ok(*id),
        ids => bail!(
            "the ADLX desktop has {} displays (Eyefinity or duplicate), expected one",
            ids.len()
        ),
    }
}

fn find_display(display_services: &Iface, id: usize) -> anyhow::Result<Iface> {
    // SAFETY: GetDisplays has an interface output.
    let list = unsafe { display_services.get(DS_GET_DISPLAYS, "GetDisplays") }?;
    for display in list.items("GetDisplays")? {
        if unique_id(&display)? == id {
            return Ok(display);
        }
    }
    bail!("ADLX has no display with the unique ID of the desktop")
}

fn version_text(version: u64) -> String {
    format!(
        "{}.{}.{}.{}",
        version >> 48,
        (version >> 32) & 0xffff,
        (version >> 16) & 0xffff,
        version & 0xffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_the_header() {
        assert_eq!(version_text(ADLX_FULL_VERSION), "2.0.0.125");
    }

    #[test]
    fn names_the_results() {
        assert_eq!(result_name(0), "ADLX_OK");
        assert_eq!(result_name(12), "ADLX_NOT_SUPPORTED");
        assert_eq!(result_name(18), "ADLX_RESET_NEEDED");
        assert_eq!(result_name(19), "ADLX result 19");
        assert_eq!(result_name(-1), "ADLX result -1");
    }

    fn rect(x: i32, y: i32) -> DesktopRect {
        DesktopRect {
            x,
            y,
            width: 1920,
            height: 1080,
        }
    }

    fn entry(x: i32, ids: &[usize]) -> DesktopEntry {
        DesktopEntry {
            rect: rect(x, 0),
            display_ids: ids.to_vec(),
        }
    }

    #[test]
    fn maps_the_desktop_by_position() {
        let entries = [entry(0, &[11]), entry(1920, &[22])];
        assert_eq!(match_desktop(&entries, rect(1920, 0)).unwrap(), 22);
        assert_eq!(match_desktop(&entries, rect(0, 0)).unwrap(), 11);
    }

    #[test]
    fn rejects_an_unclear_mapping() {
        let entries = [entry(0, &[11]), entry(0, &[22]), entry(1920, &[33, 44])];
        assert!(match_desktop(&entries, rect(0, 0)).is_err());
        assert!(match_desktop(&entries, rect(1920, 0)).is_err());
        assert!(match_desktop(&entries, rect(3840, 0)).is_err());
        assert!(match_desktop(&[entry(0, &[])], rect(0, 0)).is_err());
    }

    /// Needs an AMD GPU. Read-only: it never changes the display.
    /// Run with `cargo test -- --ignored adlx_reads_unelevated`.
    #[test]
    #[ignore]
    fn adlx_reads_unelevated() {
        let display = crate::display::displays()
            .into_iter()
            .find(|d| d.primary)
            .expect("no primary display");
        let amd = Amd::load(&display.name).expect("ADLX does not see the display");
        let native = crate::display::current_mode(&display.name).expect("no current mode");
        use super::super::CustomResBackend;
        amd.exists(native)
            .expect("cannot read the custom resolutions");
        amd.applied_resolution()
            .expect("cannot read the applied resolution");
        amd.scaling().expect("cannot read the GPU scaling");
    }
}
