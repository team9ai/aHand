//! Restricted Windows process launch and stdio capture.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::time::Duration;

use crate::sandbox::types::RuntimeExecuteResult;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, WAIT_FAILED, WAIT_TIMEOUT,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::ReadFile;
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
#[cfg(windows)]
use windows_sys::Win32::System::Pipes::CreatePipe;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

pub(super) fn spawn_restricted_capture(
    token: RawTokenHandle,
    executable: &Path,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    timeout: Duration,
) -> io::Result<RuntimeExecuteResult> {
    spawn_restricted_capture_inner(token, executable, args, cwd, env, timeout)
}

#[cfg(windows)]
type RawTokenHandle = HANDLE;
#[cfg(not(windows))]
type RawTokenHandle = usize;

#[cfg(not(windows))]
fn spawn_restricted_capture_inner(
    _: RawTokenHandle,
    _: &Path,
    _: &[String],
    _: &Path,
    _: &HashMap<String, String>,
    _: Duration,
) -> io::Result<RuntimeExecuteResult> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows restricted process launch is unavailable on this platform",
    ))
}

#[cfg(windows)]
fn spawn_restricted_capture_inner(
    token: RawTokenHandle,
    executable: &Path,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    timeout: Duration,
) -> io::Result<RuntimeExecuteResult> {
    let pipes = PipeSet::new()?;
    let job = JobGuard::new_kill_on_close()?;
    let mut startup_info: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup_info.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup_info.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
    startup_info.StartupInfo.hStdInput = pipes.stdin_read.raw();
    startup_info.StartupInfo.hStdOutput = pipes.stdout_write.raw();
    startup_info.StartupInfo.hStdError = pipes.stderr_write.raw();
    let mut desktop = super::path::string_wide_null("winsta0\\default");
    startup_info.StartupInfo.lpDesktop = desktop.as_mut_ptr();

    let mut inherited_handles = [
        pipes.stdin_read.raw(),
        pipes.stdout_write.raw(),
        pipes.stderr_write.raw(),
    ];
    for handle in inherited_handles.iter().copied() {
        set_handle_inheritable(handle)?;
    }
    let mut attributes = ProcThreadAttributeList::new(1)?;
    attributes.set_handle_list(&mut inherited_handles)?;
    startup_info.lpAttributeList = attributes.as_mut_ptr();

    let launch = launch_command(executable, args)?;
    let application_name_wide = launch
        .application_name
        .as_deref()
        .map(super::path::string_wide_null);
    let application_name_ptr = application_name_wide
        .as_ref()
        .map_or(std::ptr::null(), |wide| wide.as_ptr());
    let mut command_line = super::path::string_wide_null(&launch.command_line);
    let env_block = make_env_block(env)?;
    let cwd_wide = super::path::wide_null(cwd);
    let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    let ok = unsafe {
        CreateProcessAsUserW(
            token,
            application_name_ptr,
            command_line.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            1,
            CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED,
            env_block.as_ptr() as *mut std::ffi::c_void,
            cwd_wide.as_ptr(),
            &startup_info.StartupInfo,
            &mut process_info,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let process = ProcessGuard::new(process_info);
    let assigned = unsafe { AssignProcessToJobObject(job.raw(), process.process_handle()) };
    if assigned == 0 {
        unsafe {
            let _ = TerminateProcess(process.process_handle(), 1);
        }
        return Err(io::Error::last_os_error());
    }
    let resumed = unsafe { ResumeThread(process.thread_handle()) };
    if resumed == u32::MAX {
        unsafe {
            let _ = TerminateProcess(process.process_handle(), 1);
        }
        return Err(io::Error::last_os_error());
    }
    drop(pipes.stdin_read);
    drop(pipes.stdin_write);
    let stdout_read = pipes.stdout_read.into_raw();
    drop(pipes.stdout_write);
    let stderr_read = pipes.stderr_read.into_raw();
    drop(pipes.stderr_write);

    let stdout_thread = read_handle_to_vec(stdout_read);
    let stderr_thread = read_handle_to_vec(stderr_read);

    let wait = unsafe { WaitForSingleObject(process.process_handle(), wait_timeout_ms(timeout)) };
    if wait == WAIT_FAILED {
        unsafe {
            let _ = TerminateProcess(process.process_handle(), 1);
        }
        return Err(io::Error::last_os_error());
    }

    let timed_out = wait == WAIT_TIMEOUT;
    let exit_code = if timed_out {
        unsafe {
            let _ = TerminateProcess(process.process_handle(), 1);
        }
        None
    } else {
        let mut raw_exit: u32 = 1;
        let ok = unsafe { GetExitCodeProcess(process.process_handle(), &mut raw_exit) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Some(raw_exit as i32)
    };

    drop(job);
    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();

    Ok(RuntimeExecuteResult {
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
        exit_code,
        timed_out,
    })
}

fn make_env_block(env: &HashMap<String, String>) -> io::Result<Vec<u16>> {
    let mut items = Vec::with_capacity(env.len());
    for (key, value) in env {
        validate_env_entry(key, value)?;
        items.push((key.as_str(), value.as_str()));
    }
    items.sort_by(|(left, _), (right, _)| {
        left.to_uppercase()
            .cmp(&right.to_uppercase())
            .then(left.cmp(right))
    });

    let mut block = Vec::new();
    for (key, value) in items {
        block.extend(format!("{key}={value}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn validate_env_entry(key: &str, value: &str) -> io::Result<()> {
    if key.is_empty() || key.contains(['\0', '=']) || value.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows environment variables cannot contain NUL bytes, empty keys, or '=' in keys",
        ));
    }
    Ok(())
}

fn command_line(executable: &str, args: &[String]) -> String {
    std::iter::once(executable)
        .chain(args.iter().map(String::as_str))
        .map(quote_windows_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchCommand {
    application_name: Option<String>,
    command_line: String,
}

fn launch_command(executable: &Path, args: &[String]) -> io::Result<LaunchCommand> {
    let executable_string = executable.to_string_lossy().to_string();
    validate_command_part(&executable_string)?;
    for arg in args {
        validate_command_part(arg)?;
    }

    if is_batch_file(executable) {
        validate_batch_shell_part(&executable_string)?;
        for arg in args {
            validate_batch_shell_part(arg)?;
        }
        let cmd_exe = system_cmd_exe_path();
        return Ok(LaunchCommand {
            application_name: Some(cmd_exe.clone()),
            command_line: std::iter::once(cmd_exe.as_str())
                .chain(["/d", "/c", executable_string.as_str()])
                .chain(args.iter().map(String::as_str))
                .map(quote_windows_arg)
                .collect::<Vec<_>>()
                .join(" "),
        });
    }

    Ok(LaunchCommand {
        application_name: Some(executable_string.clone()),
        command_line: command_line(&executable_string, args),
    })
}

fn validate_command_part(value: &str) -> io::Result<()> {
    if value.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows command arguments cannot contain NUL bytes",
        ));
    }
    Ok(())
}

fn validate_batch_shell_part(value: &str) -> io::Result<()> {
    if value.contains(['%', '!']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows batch command arguments cannot contain cmd expansion markers",
        ));
    }
    Ok(())
}

fn system_cmd_exe_path() -> String {
    #[cfg(windows)]
    let system_root = std::env::var("SystemRoot")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| r"C:\Windows".to_string());

    #[cfg(not(windows))]
    let system_root = r"C:\Windows".to_string();

    format!(
        r"{}\System32\cmd.exe",
        system_root.trim_end_matches(['\\', '/'])
    )
}

fn is_batch_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

fn quote_windows_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }
    if !arg.chars().any(windows_arg_needs_quotes) {
        return arg.to_string();
    }

    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                quoted.push(ch);
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

fn windows_arg_needs_quotes(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '"' | '\\' | '&' | '|' | '<' | '>' | '^' | '(' | ')')
}

fn wait_timeout_ms(timeout: Duration) -> u32 {
    timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32
}

#[cfg(windows)]
fn set_handle_inheritable(handle: HANDLE) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};

    let ok = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
struct JobGuard {
    handle: HANDLE,
}

#[cfg(windows)]
impl JobGuard {
    fn new_kill_on_close() -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &mut limits as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            unsafe {
                CloseHandle(handle);
            }
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    fn raw(&self) -> HANDLE {
        self.handle
    }
}

#[cfg(windows)]
impl Drop for JobGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_null() {
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
struct ProcThreadAttributeList {
    buffer: Vec<u8>,
}

#[cfg(windows)]
impl ProcThreadAttributeList {
    fn new(attribute_count: u32) -> io::Result<Self> {
        let mut size = 0usize;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), attribute_count, 0, &mut size);
        }
        if size == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut buffer = vec![0u8; size];
        let ok = unsafe {
            InitializeProcThreadAttributeList(
                buffer.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST,
                attribute_count,
                0,
                &mut size,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { buffer })
    }

    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buffer.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST
    }

    fn set_handle_list(&mut self, handles: &mut [HANDLE]) -> io::Result<()> {
        let ok = unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_mut_ptr() as *const std::ffi::c_void,
                std::mem::size_of_val(handles),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for ProcThreadAttributeList {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.as_mut_ptr());
        }
    }
}

#[cfg(windows)]
struct HandleGuard {
    handle: HANDLE,
}

#[cfg(windows)]
impl HandleGuard {
    fn new(handle: HANDLE) -> Self {
        Self { handle }
    }

    fn raw(&self) -> HANDLE {
        self.handle
    }

    fn into_raw(mut self) -> HANDLE {
        let handle = self.handle;
        self.handle = std::ptr::null_mut();
        handle
    }
}

#[cfg(windows)]
impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_null() {
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
struct PipeSet {
    stdin_read: HandleGuard,
    stdin_write: HandleGuard,
    stdout_read: HandleGuard,
    stdout_write: HandleGuard,
    stderr_read: HandleGuard,
    stderr_write: HandleGuard,
}

#[cfg(windows)]
impl PipeSet {
    fn new() -> io::Result<Self> {
        let mut stdin_read = std::ptr::null_mut();
        let mut stdin_write = std::ptr::null_mut();
        let mut stdout_read = std::ptr::null_mut();
        let mut stdout_write = std::ptr::null_mut();
        let mut stderr_read = std::ptr::null_mut();
        let mut stderr_write = std::ptr::null_mut();

        unsafe {
            if CreatePipe(&mut stdin_read, &mut stdin_write, std::ptr::null_mut(), 0) == 0 {
                return Err(io::Error::last_os_error());
            }
            let stdin_read = HandleGuard::new(stdin_read);
            let stdin_write = HandleGuard::new(stdin_write);

            if CreatePipe(&mut stdout_read, &mut stdout_write, std::ptr::null_mut(), 0) == 0 {
                return Err(io::Error::last_os_error());
            }
            let stdout_read = HandleGuard::new(stdout_read);
            let stdout_write = HandleGuard::new(stdout_write);

            if CreatePipe(&mut stderr_read, &mut stderr_write, std::ptr::null_mut(), 0) == 0 {
                return Err(io::Error::last_os_error());
            }
            let stderr_read = HandleGuard::new(stderr_read);
            let stderr_write = HandleGuard::new(stderr_write);

            Ok(Self {
                stdin_read,
                stdin_write,
                stdout_read,
                stdout_write,
                stderr_read,
                stderr_write,
            })
        }
    }
}

#[cfg(windows)]
struct ProcessGuard {
    process_info: PROCESS_INFORMATION,
}

#[cfg(windows)]
impl ProcessGuard {
    fn new(process_info: PROCESS_INFORMATION) -> Self {
        Self { process_info }
    }

    fn process_handle(&self) -> HANDLE {
        self.process_info.hProcess
    }

    fn thread_handle(&self) -> HANDLE {
        self.process_info.hThread
    }
}

#[cfg(windows)]
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.process_info.hThread.is_null() {
                CloseHandle(self.process_info.hThread);
            }
            if !self.process_info.hProcess.is_null() {
                CloseHandle(self.process_info.hProcess);
            }
        }
    }
}

#[cfg(windows)]
fn read_handle_to_vec(handle: HANDLE) -> std::thread::JoinHandle<Vec<u8>> {
    let handle = handle as usize;
    std::thread::spawn(move || {
        let handle = HandleGuard::new(handle as HANDLE);
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let mut read_bytes = 0u32;
            let ok = unsafe {
                ReadFile(
                    handle.raw(),
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut read_bytes,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 || read_bytes == 0 {
                let _ = unsafe { GetLastError() };
                break;
            }
            out.extend_from_slice(&buf[..read_bytes as usize]);
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::*;

    #[test]
    fn quote_windows_arg_leaves_simple_arg_unquoted() {
        assert_eq!(quote_windows_arg("node.exe"), "node.exe");
    }

    #[test]
    fn quote_windows_arg_escapes_spaces_quotes_and_trailing_backslash() {
        assert_eq!(
            quote_windows_arg(r#"C:\Program Files\a "tool"\"#),
            r#""C:\Program Files\a \"tool\"\\""#,
        );
    }

    #[test]
    fn command_line_quotes_executable_and_args() {
        let command = command_line(
            r"C:\Program Files\node\node.exe",
            &["-e".to_string(), r#"console.log("ok")"#.to_string()],
        );

        assert_eq!(
            command,
            r#""C:\Program Files\node\node.exe" -e "console.log(\"ok\")""#,
        );
    }

    #[test]
    fn launch_command_wraps_batch_files_with_system_cmd_exe() {
        let launch =
            launch_command(Path::new(r"C:\tools\npm.cmd"), &["--version".to_string()]).unwrap();

        assert_eq!(
            launch.application_name.as_deref(),
            Some(r"C:\Windows\System32\cmd.exe")
        );
        assert_eq!(
            launch.command_line,
            r#""C:\Windows\System32\cmd.exe" /d /c "C:\tools\npm.cmd" --version"#
        );
    }

    #[test]
    fn launch_command_quotes_batch_args_with_cmd_metacharacters() {
        let launch =
            launch_command(Path::new(r"C:\tools\npm.cmd"), &["foo&whoami".to_string()]).unwrap();

        assert_eq!(
            launch.command_line,
            r#""C:\Windows\System32\cmd.exe" /d /c "C:\tools\npm.cmd" "foo&whoami""#
        );
    }

    #[test]
    fn launch_command_rejects_batch_args_with_cmd_expansion_markers() {
        let err =
            launch_command(Path::new(r"C:\tools\npm.cmd"), &["%EVIL%".to_string()]).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn env_block_is_case_insensitive_sorted_and_double_terminated() {
        let env = HashMap::from([
            ("Path".to_string(), r"C:\bin".to_string()),
            ("APP_ENV".to_string(), "test".to_string()),
        ]);
        let block = make_env_block(&env).unwrap();
        let text = String::from_utf16_lossy(&block);

        assert_eq!(text, "APP_ENV=test\0Path=C:\\bin\0\0");
    }

    #[test]
    fn env_block_rejects_nul_in_value() {
        let env = HashMap::from([("SAFE".to_string(), "ok\0EVIL=1".to_string())]);

        let err = make_env_block(&env).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn wait_timeout_ms_never_uses_infinite_sentinel() {
        assert_eq!(wait_timeout_ms(Duration::from_millis(10)), 10);
        assert_eq!(
            wait_timeout_ms(Duration::from_millis(u64::MAX)),
            u32::MAX - 1
        );
    }
}
