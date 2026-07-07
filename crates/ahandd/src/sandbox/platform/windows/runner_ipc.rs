//! Minimal Windows sandbox runner IPC.
//!
//! The parent process cannot safely create the final restricted token for a
//! dedicated sandbox account directly. It starts the current executable under
//! that account with a hidden runner argument, sends a framed request over a
//! sandbox-user-scoped named pipe, and the runner creates the restricted token
//! from its own logon token before launching the child command.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessWithLogonW, LOGON_WITH_PROFILE,
    PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess,
};

use crate::sandbox::types::{RuntimeExecuteResult, SandboxError, SandboxResult};

const RUNNER_ARG: &str = "--ahand-windows-sandbox-runner";
const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const GENERIC_READ_WRITE: u32 = 0x8000_0000 | 0x4000_0000;
const RUNNER_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RunnerRequest {
    pub(super) command: Vec<String>,
    pub(super) cwd: PathBuf,
    pub(super) env: HashMap<String, String>,
    pub(super) timeout_ms: u64,
    pub(super) capability_sid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum RunnerResponse {
    Ok(RuntimeExecuteResult),
    Err { code: String, message: String },
}

pub(super) fn try_run_from_args() -> Result<bool, String> {
    let mut args = std::env::args_os();
    let _exe = args.next();
    let Some(first) = args.next() else {
        return Ok(false);
    };
    if first != OsStr::new(RUNNER_ARG) {
        return Ok(false);
    }
    let pipe_name = args
        .next()
        .ok_or_else(|| format!("{RUNNER_ARG} requires a pipe name"))?;
    if args.next().is_some() {
        return Err(format!("{RUNNER_ARG} received unexpected extra arguments"));
    }
    run_runner(&pipe_name.to_string_lossy()).map_err(|err| err.to_string())?;
    Ok(true)
}

pub(super) fn spawn_capture(
    creds: &super::identity::SandboxCreds,
    request: RunnerRequest,
) -> SandboxResult<RuntimeExecuteResult> {
    let pipe_name = format!(r"\\.\pipe\ahand-sandbox-runner-{}", Uuid::new_v4());
    let pipe = create_runner_pipe(&pipe_name, &creds.username).map_err(|err| {
        SandboxError::unavailable(format!(
            "failed to create Windows sandbox runner pipe: {err}"
        ))
    })?;
    let process = match spawn_runner_process(creds, &pipe_name, &request.cwd) {
        Ok(process) => process,
        Err(err) => {
            unsafe {
                CloseHandle(pipe);
            }
            return Err(SandboxError::unavailable(format!(
                "failed to launch Windows sandbox runner: {err}"
            )));
        }
    };

    let result = (|| -> SandboxResult<RuntimeExecuteResult> {
        connect_pipe_with_timeout(pipe).map_err(|err| {
            SandboxError::unavailable(format!(
                "Windows sandbox runner did not connect to IPC pipe: {err}"
            ))
        })?;
        let mut pipe_file = unsafe { File::from_raw_handle(pipe as _) };
        write_frame(&mut pipe_file, &request).map_err(|err| {
            SandboxError::unavailable(format!(
                "failed to send Windows sandbox runner request: {err}"
            ))
        })?;
        let response: RunnerResponse = read_frame(&mut pipe_file).map_err(|err| {
            SandboxError::unavailable(format!(
                "failed to read Windows sandbox runner response: {err}"
            ))
        })?;
        match response {
            RunnerResponse::Ok(result) => Ok(result),
            RunnerResponse::Err { code, message } => Err(SandboxError::new(code, message)),
        }
    })();

    unsafe {
        if !process.hThread.is_null() {
            CloseHandle(process.hThread);
        }
        if !process.hProcess.is_null() {
            if result.is_err() {
                let _ = TerminateProcess(process.hProcess, 1);
            }
            CloseHandle(process.hProcess);
        }
    }

    result
}

fn run_runner(pipe_name: &str) -> io::Result<()> {
    let pipe = open_runner_pipe(pipe_name)?;
    let mut pipe_file = unsafe { File::from_raw_handle(pipe as _) };
    let request: RunnerRequest = read_frame(&mut pipe_file)?;
    let response = match run_runner_request(request) {
        Ok(result) => RunnerResponse::Ok(result),
        Err(err) => RunnerResponse::Err {
            code: err.code,
            message: err.message,
        },
    };
    write_frame(&mut pipe_file, &response)
}

fn run_runner_request(request: RunnerRequest) -> SandboxResult<RuntimeExecuteResult> {
    let token = super::token::create_for_sandbox_user_sid_string(&request.capability_sid).map_err(
        |err| SandboxError::unavailable(format!("failed to create restricted token: {err}")),
    )?;
    let Some((executable, args)) = request.command.split_first() else {
        return Err(SandboxError::invalid_command(
            "Windows sandbox runner command must not be empty",
        ));
    };
    super::process::spawn_restricted_capture(
        token.handle(),
        Path::new(executable),
        args,
        &request.cwd,
        &request.env,
        Duration::from_millis(request.timeout_ms),
    )
    .map_err(|err| {
        SandboxError::unavailable(format!("Windows sandbox process launch failed: {err}"))
    })
}

fn create_runner_pipe(name: &str, sandbox_username: &str) -> io::Result<HANDLE> {
    let sandbox_sid = super::sandbox_users::resolve_sandbox_user_sid(sandbox_username)
        .map_err(|err| io::Error::new(io::ErrorKind::PermissionDenied, err.to_string()))?;
    let sddl = wide_null(&format!("D:(A;;GA;;;{sandbox_sid})"));
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut sd,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut security_attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let name_w = wide_null(name);
    let handle = unsafe {
        CreateNamedPipeW(
            name_w.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            65536,
            65536,
            0,
            &mut security_attributes,
        )
    };
    unsafe {
        LocalFree(sd as HLOCAL);
    }
    if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(handle)
}

fn open_runner_pipe(name: &str) -> io::Result<HANDLE> {
    let name_w = wide_null(name);
    let handle = unsafe {
        CreateFileW(
            name_w.as_ptr(),
            GENERIC_READ_WRITE,
            0,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(handle)
}

fn connect_pipe_with_timeout(pipe: HANDLE) -> io::Result<()> {
    let (tx, rx) = mpsc::sync_channel(1);
    let pipe_value = pipe as usize;
    std::thread::spawn(move || {
        let pipe = pipe_value as HANDLE;
        let ok = unsafe { ConnectNamedPipe(pipe, ptr::null_mut()) };
        if ok != 0 {
            let _ = tx.send(Ok(()));
            return;
        }
        let err = unsafe { GetLastError() };
        const ERROR_PIPE_CONNECTED: u32 = 535;
        if err == ERROR_PIPE_CONNECTED {
            let _ = tx.send(Ok(()));
        } else {
            let _ = tx.send(Err(io::Error::from_raw_os_error(err as i32)));
        }
    });
    rx.recv_timeout(RUNNER_CONNECT_TIMEOUT)
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "runner pipe connect timed out"))?
}

fn spawn_runner_process(
    creds: &super::identity::SandboxCreds,
    pipe_name: &str,
    cwd: &Path,
) -> io::Result<PROCESS_INFORMATION> {
    let exe = std::env::current_exe()?;
    let exe_string = exe.to_string_lossy().to_string();
    let command_line = format!(
        "{} {} {}",
        quote_windows_arg(&exe_string),
        quote_windows_arg(RUNNER_ARG),
        quote_windows_arg(pipe_name)
    );
    let mut command_line_w = wide_null(&command_line);
    let exe_w = wide_null(&exe_string);
    let cwd_w = super::path::process_cwd_wide_null(cwd);
    let user_w = wide_null(&creds.username);
    let domain_w = wide_null(".");
    let password_w = wide_null(&creds.password);
    let mut startup_info: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup_info.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        CreateProcessWithLogonW(
            user_w.as_ptr(),
            domain_w.as_ptr(),
            password_w.as_ptr(),
            LOGON_WITH_PROFILE,
            exe_w.as_ptr(),
            command_line_w.as_mut_ptr(),
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            ptr::null(),
            cwd_w.as_ptr(),
            &startup_info,
            &mut process_info,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(process_info)
}

fn read_frame<T: for<'de> Deserialize<'de>>(reader: &mut File) -> io::Result<T> {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    let mut bytes = vec![0u8; len];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn write_frame<T: Serialize>(writer: &mut File, value: &T) -> io::Result<()> {
    let bytes =
        serde_json::to_vec(value).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
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

fn wide_null(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_request_frame_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("frame.bin");
        let request = RunnerRequest {
            command: vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "echo ok".to_string(),
            ],
            cwd: PathBuf::from(r"C:\work"),
            env: HashMap::from([("A".to_string(), "B".to_string())]),
            timeout_ms: 1000,
            capability_sid: "S-1-5-21-1-2-3-4".to_string(),
        };

        {
            let mut file = File::create(&path).unwrap();
            write_frame(&mut file, &request).unwrap();
        }
        let mut file = File::open(&path).unwrap();
        let decoded: RunnerRequest = read_frame(&mut file).unwrap();

        assert_eq!(decoded.command, request.command);
        assert_eq!(decoded.cwd, request.cwd);
        assert_eq!(decoded.env, request.env);
        assert_eq!(decoded.timeout_ms, request.timeout_ms);
        assert_eq!(decoded.capability_sid, request.capability_sid);
    }

    #[test]
    fn quote_windows_arg_escapes_spaces_quotes_and_trailing_backslash() {
        assert_eq!(
            quote_windows_arg(r#"C:\Program Files\a "tool"\"#),
            r#""C:\Program Files\a \"tool\"\\""#,
        );
    }
}
