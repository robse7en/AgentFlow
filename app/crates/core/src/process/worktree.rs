//! Find and stop processes whose working directory is inside a worktree (Windows), ported from
//! `backend/agentflow/services/worktree_processes.py`.
//!
//! Windows refuses to delete a directory that any process has as its working directory, so a
//! shell or agent the human started inside a task's worktree (which AgentFlow doesn't track)
//! blocks cleanup. Nothing exposes another process's working directory, so this reads it from the
//! process's PEB, as Process Explorer does. POSIX systems delete such directories without
//! complaint, so there this finds nothing.
//!
//! Guards beyond the Python version, because a containment mistake here kills unrelated work:
//! the directory must exist, be a directory, and not be a filesystem root.

use std::path::Path;

use super::ProcessError;
use crate::paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProcess {
    pub pid: u32,
    pub name: String,
    pub cwd: String,
}

/// Kill every process working inside `directory` and return the ones that were killed. Never
/// touches this process or its ancestors: killing a shell that launched the backend would take
/// the backend down with it.
///
/// Callers must pass only a task worktree directly inside the managed worktree directory, as the
/// Python engine does: the only guard here is against filesystem roots, so a home folder would
/// be cleared too.
pub async fn stop_processes_in(directory: &Path) -> Result<Vec<WorktreeProcess>, ProcessError> {
    let directory = directory.to_path_buf();
    if !directory.is_dir() {
        return Err(ProcessError(format!("Not a directory: {}", directory.display())));
    }
    let target = paths::resolve(&directory);
    if paths::is_root(&target) {
        return Err(ProcessError(format!("Refusing to stop processes in a filesystem root: {}", target.display())));
    }
    tokio::task::spawn_blocking(move || platform::stop_processes_in(&target))
        .await
        .map_err(|e| ProcessError(format!("process scan failed: {e}")))
}

/// A process that probably still has a worktree open: working inside it, or started with it on its
/// command line (an editor with the folder open holds it without working there). Only reported,
/// never stopped: closing someone's editor is their call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeHolder {
    pub pid: u32,
    pub name: String,
}

/// The processes that look like they hold `directory` (best effort: a window opened on the folder
/// after the editor started leaves no trace on its command line).
pub async fn find_holders(directory: &Path) -> Result<Vec<WorktreeHolder>, ProcessError> {
    let target = paths::resolve(directory);
    if paths::is_root(&target) {
        return Err(ProcessError(format!("Refusing to look for processes in a filesystem root: {}", target.display())));
    }
    tokio::task::spawn_blocking(move || platform::find_holders(&target))
        .await
        .map_err(|e| ProcessError(format!("process scan failed: {e}")))
}

/// Whether `command_line` names `directory` itself or something inside it, ignoring case, slash
/// direction and a `\\?\` prefix. `DE-1` is not found in `DE-12`.
pub(crate) fn mentions(command_line: &str, directory: &Path) -> bool {
    fn normalized(text: &str) -> String {
        let text = text.replace('/', "\\").to_lowercase();
        text.replace("\\\\?\\", "")
    }
    let command_line = normalized(command_line);
    let directory = normalized(&directory.to_string_lossy());
    let directory = directory.trim_end_matches('\\');
    if directory.is_empty() {
        return false;
    }
    command_line.match_indices(directory).any(|(start, found)| {
        command_line[start + found.len()..].chars().next().is_none_or(|next| matches!(next, '\\' | '"' | '\'' | ' '))
    })
}

/// What the engine calls to free a worktree directory: [`SystemKiller`] in the app, a fake in
/// tests (the Python tests monkeypatched `stop_processes_in`).
#[async_trait::async_trait]
pub trait WorktreeKiller: Send + Sync {
    async fn stop_processes_in(&self, directory: &Path) -> Result<Vec<WorktreeProcess>, ProcessError>;

    /// Who still holds the directory once removal has failed, to tell the human what to close.
    async fn holders(&self, _directory: &Path) -> Result<Vec<WorktreeHolder>, ProcessError> {
        Ok(Vec::new())
    }
}

/// Kills real processes ([`stop_processes_in`]).
pub struct SystemKiller;

#[async_trait::async_trait]
impl WorktreeKiller for SystemKiller {
    async fn stop_processes_in(&self, directory: &Path) -> Result<Vec<WorktreeProcess>, ProcessError> {
        stop_processes_in(directory).await
    }

    async fn holders(&self, directory: &Path) -> Result<Vec<WorktreeHolder>, ProcessError> {
        find_holders(directory).await
    }
}

#[cfg(not(all(windows, target_pointer_width = "64")))]
mod platform {
    use super::*;

    pub fn stop_processes_in(_: &Path) -> Vec<WorktreeProcess> {
        Vec::new()
    }

    pub fn find_holders(_: &Path) -> Vec<WorktreeHolder> {
        Vec::new()
    }
}

#[cfg(all(windows, target_pointer_width = "64"))]
mod platform {
    use std::collections::{HashMap, HashSet};
    use std::ffi::c_void;

    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        IsWow64Process, OpenProcess, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE, PROCESS_VM_READ, TerminateProcess, WaitForSingleObject,
    };

    use super::*;

    /// PEB.ProcessParameters and RTL_USER_PROCESS_PARAMETERS.CurrentDirectory.DosPath / .CommandLine (x64).
    const PEB_PROCESS_PARAMETERS: usize = 0x20;
    const PARAMS_CURRENT_DIRECTORY: usize = 0x38;
    const PARAMS_COMMAND_LINE: usize = 0x70;

    /// Processes with the directory as their working directory or on their command line. Own
    /// ancestry is skipped, as when stopping.
    pub fn find_holders(target: &Path) -> Vec<WorktreeHolder> {
        let snapshot = snapshot();
        let spared = ancestry(&snapshot);
        let mut holders = Vec::new();
        for (&pid, (_, name)) in &snapshot {
            if pid <= 4 || spared.contains(&pid) {
                continue;
            }
            let Some(found) = inspect_process(pid) else { continue };
            if works_inside(&found.cwd, target) || mentions(&found.command_line, target) {
                holders.push(WorktreeHolder { pid, name: name.clone() });
            }
        }
        holders.sort_by_key(|h| h.pid);
        holders
    }

    pub fn stop_processes_in(target: &Path) -> Vec<WorktreeProcess> {
        let mut stopped = Vec::new();
        // Kills land in arbitrary order, so a parent (an agent CLI) can relaunch a child it just
        // lost (an MCP server) into the same directory; rescan until a pass finds nothing.
        for _ in 0..3 {
            let snapshot = snapshot();
            let spared = ancestry(&snapshot);
            let mut killed = Vec::new();
            for (&pid, (_, name)) in &snapshot {
                if pid <= 4 || spared.contains(&pid) {
                    continue; // 0 and 4 are the idle and System processes
                }
                if let Some(found) = inspect(pid, name)
                    && works_inside(&found.cwd, target)
                    && terminate(pid)
                {
                    killed.push(found);
                }
            }
            if killed.is_empty() {
                break;
            }
            stopped.extend(killed);
        }
        stopped
    }

    /// A PEB keeps the working directory as the process was given it, so one started through an
    /// 8.3 short name (`...\ROBROD~1\...`) only matches the resolved, long-form target once its
    /// own path is resolved too. The lexical check comes first: it needs no filesystem access.
    fn works_inside(cwd: &str, target: &Path) -> bool {
        let cwd = Path::new(cwd);
        paths::is_within(cwd, target) || paths::is_within(&paths::resolve(cwd), target)
    }

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful Open/Create call and is closed once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// pid -> (parent pid, executable name) for every running process.
    fn snapshot() -> HashMap<u32, (u32, String)> {
        let mut entries = HashMap::new();
        // SAFETY: Toolhelp snapshot iteration with a correctly sized entry.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap.is_null() || snap == INVALID_HANDLE_VALUE {
                return entries;
            }
            let snap = Handle(snap);
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = Process32FirstW(snap.0, &mut entry);
            while more != 0 {
                let length = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
                entries.insert(entry.th32ProcessID, (entry.th32ParentProcessID, name));
                more = Process32NextW(snap.0, &mut entry);
            }
        }
        entries
    }

    fn ancestry(snapshot: &HashMap<u32, (u32, String)>) -> HashSet<u32> {
        let mut spared = HashSet::new();
        let mut pid = std::process::id();
        while pid != 0 && spared.insert(pid) {
            pid = snapshot.get(&pid).map_or(0, |(parent, _)| *parent);
        }
        spared
    }

    fn read(process: HANDLE, address: usize, size: usize) -> Option<Vec<u8>> {
        let mut buffer = vec![0u8; size];
        let mut read = 0usize;
        // SAFETY: the buffer is `size` bytes; ReadProcessMemory reports how much it filled.
        let ok = unsafe {
            ReadProcessMemory(process, address as *const c_void, buffer.as_mut_ptr().cast(), size, &mut read)
        };
        if ok == 0 {
            return None;
        }
        buffer.truncate(read);
        Some(buffer)
    }

    fn pointer(bytes: &[u8]) -> usize {
        usize::from_le_bytes(bytes[..8].try_into().expect("8 bytes"))
    }

    /// The process's working directory, or None when it can't be read (another user's or an
    /// elevated process, a 32-bit one whose live directory sits in a separate 32-bit PEB).
    fn inspect(pid: u32, name: &str) -> Option<WorktreeProcess> {
        inspect_process(pid).map(|p| WorktreeProcess { pid, name: name.to_string(), cwd: p.cwd })
    }

    struct Inspected {
        cwd: String,
        /// Empty when it couldn't be read.
        command_line: String,
    }

    /// Reads a UNICODE_STRING at `offset` in the process parameters at `parameters`.
    fn unicode_string(process: HANDLE, parameters: usize, offset: usize) -> Option<String> {
        let string = read(process, parameters + offset, 16).filter(|s| s.len() == 16)?;
        let length = u16::from_le_bytes([string[0], string[1]]) as usize;
        if length == 0 {
            return None;
        }
        let text = read(process, pointer(&string[8..16]), length)?;
        let units: Vec<u16> = text.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        Some(String::from_utf16_lossy(&units))
    }

    fn inspect_process(pid: u32) -> Option<Inspected> {
        // SAFETY: handles are wrapped for closing; structures are zeroed and sized.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
            if process.is_null() {
                return None;
            }
            let process = Handle(process);
            let mut wow64 = 0;
            if IsWow64Process(process.0, &mut wow64) == 0 || wow64 != 0 {
                return None;
            }
            let mut info: PROCESS_BASIC_INFORMATION = std::mem::zeroed();
            let status = NtQueryInformationProcess(
                process.0,
                ProcessBasicInformation,
                (&mut info as *mut PROCESS_BASIC_INFORMATION).cast(),
                std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
                std::ptr::null_mut(),
            );
            if status != 0 || info.PebBaseAddress.is_null() {
                return None;
            }
            let parameters =
                read(process.0, info.PebBaseAddress as usize + PEB_PROCESS_PARAMETERS, 8).filter(|r| r.len() == 8)?;
            let parameters = pointer(&parameters);
            let cwd = unicode_string(process.0, parameters, PARAMS_CURRENT_DIRECTORY)?;
            let command_line = unicode_string(process.0, parameters, PARAMS_COMMAND_LINE).unwrap_or_default();
            Some(Inspected { cwd, command_line })
        }
    }

    /// Kill the process and wait (briefly) for it to exit so its directory handle is released.
    fn terminate(pid: u32) -> bool {
        // SAFETY: the handle is wrapped for closing.
        unsafe {
            let process = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid);
            if process.is_null() {
                return false;
            }
            let process = Handle(process);
            if TerminateProcess(process.0, 1) == 0 {
                return false;
            }
            WaitForSingleObject(process.0, 5000);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_line_names_the_directory_or_something_inside_it() {
        let dir = Path::new(r"D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-15400");
        let code = r#""C:\Program Files\Microsoft VS Code\Code.exe" "D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-15400\Acme.Web""#;
        assert!(mentions(code, dir));
        assert!(mentions(r"node D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-15400", dir));
        assert!(mentions("code d:/dev/agentflow/data/.agentflow/worktrees/af-15400/src", dir));
        assert!(mentions(r"tool \\?\D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-15400 --x", dir));
    }

    #[test]
    fn a_longer_name_or_another_directory_is_not_a_mention() {
        let dir = Path::new(r"D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-15400");
        assert!(!mentions(r"code D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-154001", dir));
        assert!(!mentions(r"code D:\Dev\AgentFlow\data\.agentflow\worktrees\AF-15121", dir));
        assert!(!mentions("", dir));
        assert!(!mentions("code anything", Path::new("")));
    }
}
