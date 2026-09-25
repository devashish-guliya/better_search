use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};

/// Lets Windows move this process's memory out of RAM. It stays reserved and is read
/// back in on first use, which costs a few hundred milliseconds at most.
pub fn trim_working_set() {
    // SAFETY: the pseudo handle needs no closing; (MAX, MAX) is the documented way to
    // ask for the working set to be trimmed.
    unsafe {
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

pub struct ProcessMemory {
    pub private: usize,
    pub working_set: usize,
    pub peak_working_set: usize,
}

pub fn process_memory() -> Option<ProcessMemory> {
    // SAFETY: zero is a valid bit pattern for this plain C struct.
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    let size = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    counters.cb = size;
    // SAFETY: the pseudo handle needs no closing; the struct size is passed in `cb`.
    let ok =
        unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut counters).cast(), size) };
    (ok != 0).then_some(ProcessMemory {
        private: counters.PrivateUsage,
        working_set: counters.WorkingSetSize,
        peak_working_set: counters.PeakWorkingSetSize,
    })
}
