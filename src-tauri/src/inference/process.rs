//! Keeps the inference server from outliving Luna.
use std::fs;
use std::io;
use std::path::PathBuf;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::process::Child;

/// Records the running server's process ID so a server left behind by a crash can be stopped.
pub struct PidFile(PathBuf);

impl PidFile {
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn record(&self, pid: u32) {
        if let Err(error) = fs::write(&self.0, pid.to_string()) {
            log::warn!("failed to record llama-server process: {error}");
        }
    }

    pub fn clear(&self) {
        if let Err(error) = fs::remove_file(&self.0)
            && error.kind() != io::ErrorKind::NotFound
        {
            log::warn!("failed to remove llama-server process record: {error}");
        }
    }

    /// Stops a server from a previous run that was not shut down, if one is still running.
    pub fn stop_leftover(&self) {
        let Some(pid) = fs::read_to_string(&self.0)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        else {
            return;
        };
        let pid = Pid::from_u32(pid);
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        // Process IDs are reused, so only stop the process if it is still llama-server.
        if let Some(process) = system.process(pid)
            && process.name().to_string_lossy().starts_with("llama-server")
        {
            log::warn!("stopping llama-server left over from a previous run");
            process.kill();
        }
        self.clear();
    }
}

/// On Windows the server joins a job that the OS closes, killing it, when Luna exits.
#[cfg(windows)]
pub fn bind_to_luna(child: &Child) -> io::Result<()> {
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    // The job handle stays open for Luna's lifetime and is closed by the OS on exit.
    static JOB: OnceLock<usize> = OnceLock::new();
    let job = match JOB.get() {
        Some(job) => *job as HANDLE,
        None => {
            // SAFETY: plain Win32 calls with valid pointers to stack values.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                return Err(io::Error::last_os_error());
            }
            *JOB.get_or_init(|| job as usize) as HANDLE
        }
    };
    let process = child
        .raw_handle()
        .ok_or_else(|| io::Error::other("llama-server already exited"))?;
    // SAFETY: both handles are valid for the duration of the call.
    if unsafe { AssignProcessToJobObject(job, process as HANDLE) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// On macOS a clean quit unloads the model and leftovers are stopped on the next launch.
#[cfg(not(windows))]
pub fn bind_to_luna(_: &Child) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leftover_record_for_another_program_is_cleared_without_killing_it() {
        let path = std::env::temp_dir().join(format!("luna-pid-{}", std::process::id()));
        let record = PidFile::new(path.clone());
        record.record(std::process::id());
        record.stop_leftover();
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn leftover_llama_server_is_stopped() {
        let dir = std::env::temp_dir().join(format!("luna-leftover-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fake_server = dir.join("llama-server");
        fs::copy("/bin/sleep", &fake_server).unwrap();
        let mut child = tokio::process::Command::new(&fake_server)
            .arg("30")
            .spawn()
            .unwrap();
        let record = PidFile::new(dir.join("llama-server.pid"));
        record.record(child.id().unwrap());

        record.stop_leftover();

        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .expect("leftover server was not stopped")
            .unwrap();
        assert!(!status.success());
        fs::remove_dir_all(dir).unwrap();
    }
}
