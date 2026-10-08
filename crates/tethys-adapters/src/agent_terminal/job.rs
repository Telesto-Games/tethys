//! A Windows Job Object that kills the whole process tree when dropped.
//!
//! Closing a ConPTY doesn't kill the agent's children, so each session's root
//! process is put in a kill-on-close job.

#[cfg(windows)]
pub use imp::Job;

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::io;
    use std::mem;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    };

    pub struct Job(HANDLE);

    // SAFETY: a job handle is a kernel handle, usable from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> io::Result<Job> {
            // SAFETY: plain Win32 calls; the handle is owned by `Job` and closed on drop.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let job = Job(handle);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const c_void,
                    mem::size_of_val(&info) as u32,
                );
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(job)
            }
        }

        /// Adds a process (by handle) to the job. Its future children join too.
        pub fn assign(&self, process: *mut c_void) -> io::Result<()> {
            // SAFETY: `process` is a live process handle owned by the PTY.
            if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// How many processes are still running in the job.
        pub fn active_processes(&self) -> u32 {
            // SAFETY: valid job handle; `info` is the struct for this info class.
            unsafe {
                let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = mem::zeroed();
                let ok = QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    &mut info as *mut _ as *mut c_void,
                    mem::size_of_val(&info) as u32,
                    std::ptr::null_mut(),
                );
                if ok == 0 { 0 } else { info.ActiveProcesses }
            }
        }

        /// Kills every process in the job.
        pub fn terminate(&self) {
            // SAFETY: valid job handle.
            unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: we own the handle. Closing the last handle kills the tree.
            unsafe { CloseHandle(self.0) };
        }
    }
}
