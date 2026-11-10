//! The macOS reader. It is the only module with unsafe code.
//!
//! The invariant: each unsafe call writes only into a buffer that this
//! module owns, and passes that buffer's exact length, so the kernel or libc
//! never writes past its end. Every name passed to `sysctlbyname` is a
//! NUL-terminated `CStr`, and no call passes a new value to the kernel. The
//! libproc calls and `mach_timebase_info` only read, and write only into a
//! buffer that this module owns.

use std::collections::BTreeSet;
use std::ffi::CStr;
use std::io;
use std::sync::OnceLock;
use std::time::Duration;

use super::sysctl::{
    PRESSURE_BYTES, PRESSURE_SYSCTL, SWAP_SYSCTL, SWAP_USAGE_BYTES, parse_pressure, parse_swap,
    ticks,
};
use super::{ProcessUse, Sample, TreeUnreadable, Unreadable};

// The parser reads `vm.swapusage` at the length of macOS's `xsw_usage`.
const _: () = assert!(size_of::<libc::xsw_usage>() == SWAP_USAGE_BYTES);
const _: () = assert!(size_of::<libc::c_int>() == PRESSURE_BYTES);

/// The child process IDs that the first call to `proc_listchildpids` has room
/// for.
const FIRST_CHILDREN: usize = 256;

/// The most child process IDs that one process is read for. A process with
/// more keeps only this many in its tree.
const MAX_CHILDREN: usize = 65_536;

/// Takes one sample of the machine.
pub(super) fn sample() -> Result<Sample, Unreadable> {
    let swap = parse_swap(&sysctl(SWAP_SYSCTL, SWAP_USAGE_BYTES)?)?;
    Ok(Sample {
        load_1m: load_1m()?,
        memory_pressure: parse_pressure(&sysctl(PRESSURE_SYSCTL, PRESSURE_BYTES)?)?,
        swap_used_bytes: swap.used_bytes,
        swap_total_bytes: swap.total_bytes,
    })
}

/// Returns the 1-minute load average.
fn load_1m() -> Result<f64, Unreadable> {
    let mut averages = [0.0_f64; 1];
    // SAFETY: `averages` holds one f64, and getloadavg writes at most the
    // one value that `nelem` asks for.
    let written = unsafe { libc::getloadavg(averages.as_mut_ptr(), 1) };
    match (written, averages.first()) {
        (1, Some(load)) => Ok(*load),
        _ => Err(Unreadable::Load),
    }
}

/// Returns the value of the sysctl `name`, which is at most `len` bytes.
fn sysctl(name: &'static CStr, len: usize) -> Result<Vec<u8>, Unreadable> {
    let mut value = vec![0_u8; len];
    let mut written = value.len();
    // SAFETY: `name` is NUL-terminated. `value` is writable for `written`
    // bytes, and sysctlbyname writes at most that many and stores the count
    // it wrote back in `written`. The new value is null with length 0, so
    // the call changes nothing in the kernel.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            &raw mut written,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return Err(Unreadable::Sysctl {
            name,
            error: std::io::Error::last_os_error(),
        });
    }
    value.truncate(written);
    Ok(value)
}

/// Returns the Mach clock's timebase, as `numer` over `denom` nanoseconds a
/// tick.
#[expect(
    deprecated,
    reason = "libc marks its Mach declarations as deprecated in favour of the mach2 crate, and the eval /evals/deadband-process-reading kept this change on libc"
)]
fn timebase() -> (u32, u32) {
    static TIMEBASE: OnceLock<(u32, u32)> = OnceLock::new();
    *TIMEBASE.get_or_init(|| {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `info` is a writable mach_timebase_info, which is all the
        // call writes.
        let status = unsafe { libc::mach_timebase_info(&raw mut info) };
        if status == 0 {
            (info.numer, info.denom)
        } else {
            (1, 1)
        }
    })
}

/// Returns the CPU time and disk bytes of `pid`, or `None` when it is gone.
/// A process that this user may not read reads as zero.
fn usage(pid: libc::c_int) -> Result<Option<(Duration, u64)>, TreeUnreadable> {
    // SAFETY: rusage_info_v4 holds only integers, so all zeros is a value.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a writable rusage_info_v4, and the flavor asks the
    // kernel for exactly that struct. The call takes the buffer's address
    // through its `rusage_info_t *` parameter.
    let status = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            (&raw mut info).cast::<libc::rusage_info_t>(),
        )
    };
    if status == 0 {
        let (numer, denom) = timebase();
        let cpu = ticks(
            info.ri_user_time.saturating_add(info.ri_system_time),
            numer,
            denom,
        );
        let disk = info
            .ri_diskio_bytesread
            .saturating_add(info.ri_diskio_byteswritten);
        return Ok(Some((cpu, disk)));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(None),
        Some(libc::EPERM) => Ok(Some((Duration::ZERO, 0))),
        _ => Err(TreeUnreadable::Failed(error)),
    }
}

/// Returns the child process IDs of `pid`. A process that is gone has none.
fn children(pid: libc::c_int) -> Result<Vec<libc::c_int>, TreeUnreadable> {
    let mut room = FIRST_CHILDREN;
    loop {
        let mut found: Vec<libc::c_int> = vec![0; room];
        let bytes = room.saturating_mul(size_of::<libc::c_int>());
        let bytes = libc::c_int::try_from(bytes).unwrap_or(libc::c_int::MAX);
        // SAFETY: `found` is writable for `bytes` bytes, and the call writes
        // at most that many.
        let count = unsafe { libc::proc_listchildpids(pid, found.as_mut_ptr().cast(), bytes) };
        let Ok(count) = usize::try_from(count) else {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ESRCH) => Ok(Vec::new()),
                _ => Err(TreeUnreadable::Failed(error)),
            };
        };
        if count < room || room >= MAX_CHILDREN {
            found.truncate(count);
            return Ok(found);
        }
        room = room.saturating_mul(2);
    }
}

/// Reads each process of the tree under `root`.
pub(super) fn tree(root: u32) -> Result<Vec<ProcessUse>, TreeUnreadable> {
    let root = libc::c_int::try_from(root)
        .map_err(|_| TreeUnreadable::Failed(io::ErrorKind::InvalidInput.into()))?;
    let mut seen = BTreeSet::from([root]);
    let mut pending = vec![root];
    let mut processes = Vec::new();
    while let Some(pid) = pending.pop() {
        let Some((cpu, disk_bytes)) = usage(pid)? else {
            continue;
        };
        if let Ok(pid) = u32::try_from(pid) {
            processes.push(ProcessUse {
                pid,
                cpu,
                disk_bytes,
            });
        }
        pending.extend(
            children(pid)?
                .into_iter()
                .filter(|child| seen.insert(*child)),
        );
    }
    Ok(processes)
}
