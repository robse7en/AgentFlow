//! A Windows Job Object that kills every process assigned to it when the job is dropped or the
//! owning process exits, however it exits. For the embedded-terminal backend:
//! agents started in a job cannot outlive the app.
//!
//! Not used by the zellij path, whose sessions deliberately outlive the backend.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

use super::ProcessError;

pub struct Job {
    handle: HANDLE,
}

// SAFETY: a job handle may be used and closed from any thread.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// A new anonymous job whose processes are killed when its last handle closes.
    pub fn kill_on_close() -> Result<Job, ProcessError> {
        // SAFETY: plain Win32 calls with valid arguments; the handle is owned by the returned Job.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(ProcessError(format!("CreateJobObject failed: {}", std::io::Error::last_os_error())));
            }
            let job = Job { handle };
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set == 0 {
                return Err(ProcessError(format!(
                    "SetInformationJobObject failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(job)
        }
    }

    /// Put a running process (and so every process it starts afterwards) into the job.
    pub fn assign(&self, pid: u32) -> Result<(), ProcessError> {
        // SAFETY: the process handle is closed before returning; failures are reported.
        unsafe {
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return Err(ProcessError(format!("OpenProcess({pid}) failed: {}", std::io::Error::last_os_error())));
            }
            let assigned = AssignProcessToJobObject(self.handle, process);
            let error = std::io::Error::last_os_error();
            CloseHandle(process);
            if assigned == 0 {
                return Err(ProcessError(format!("AssignProcessToJobObject({pid}) failed: {error}")));
            }
        }
        Ok(())
    }

    /// Kill every process in the job now (dropping the job does too, once no other handle to it
    /// is open).
    pub fn terminate(&self, exit_code: u32) {
        // SAFETY: the handle is valid for the lifetime of self.
        unsafe {
            TerminateJobObject(self.handle, exit_code);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle was created by this Job and is closed exactly once.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_job_kills_its_processes() {
        let mut child = std::process::Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let job = Job::kill_on_close().unwrap();
        job.assign(child.id()).unwrap();
        assert!(child.try_wait().unwrap().is_none());
        drop(job);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline, "the job did not kill its process");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
