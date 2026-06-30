// Элевация на Windows: реинвок самого приложения через ShellExecuteExW("runas")
// (UAC-диалог) и проверка текущего токена на «повышенность».

use crate::logging;

/// Запущены ли мы с правами администратора.
pub fn is_elevated() -> bool {
    use std::ffi::c_void;
    #[repr(C)]
    struct TokenElevation {
        token_is_elevated: u32,
    }
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION: i32 = 20;
    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: *mut c_void, desired: u32, token: *mut *mut c_void) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            class: i32,
            info: *mut c_void,
            len: u32,
            ret_len: *mut u32,
        ) -> i32;
    }
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(h: *mut c_void) -> i32;
    }
    unsafe {
        let mut token: *mut c_void = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elev = TokenElevation {
            token_is_elevated: 0,
        };
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TOKEN_ELEVATION,
            &mut elev as *mut _ as *mut c_void,
            std::mem::size_of::<TokenElevation>() as u32,
            &mut ret,
        );
        CloseHandle(token);
        ok != 0 && elev.token_is_elevated != 0
    }
}

/// Перезапустить наш exe с правами администратора и дождаться завершения.
/// Возвращает код выхода элевированного процесса.
pub fn run_elevated_self(args: &[&str]) -> Result<i32, String> {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // Параметры: каждый аргумент с пробелом — в кавычках.
    let params: String = args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    let verb = wide(OsStr::new("runas"));
    let file = wide(exe.as_os_str());
    let params_w = wide(OsStr::new(&params));

    #[repr(C)]
    struct ShellExecuteInfoW {
        cb_size: u32,
        f_mask: u32,
        hwnd: *mut c_void,
        lp_verb: *const u16,
        lp_file: *const u16,
        lp_parameters: *const u16,
        lp_directory: *const u16,
        n_show: i32,
        h_inst_app: *mut c_void,
        lp_id_list: *mut c_void,
        lp_class: *const u16,
        hkey_class: *mut c_void,
        dw_hot_key: u32,
        h_icon: *mut c_void,
        h_process: *mut c_void,
    }
    const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
    const SW_HIDE: i32 = 0;
    const INFINITE: u32 = 0xFFFF_FFFF;

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
    }
    extern "system" {
        fn WaitForSingleObject(h: *mut c_void, ms: u32) -> u32;
        fn GetExitCodeProcess(h: *mut c_void, code: *mut u32) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    logging::info("elevate", format!("реинвок (UAC): {}", args.join(" ")));
    unsafe {
        let mut info: ShellExecuteInfoW = std::mem::zeroed();
        info.cb_size = std::mem::size_of::<ShellExecuteInfoW>() as u32;
        info.f_mask = SEE_MASK_NOCLOSEPROCESS;
        info.lp_verb = verb.as_ptr();
        info.lp_file = file.as_ptr();
        info.lp_parameters = params_w.as_ptr();
        info.n_show = SW_HIDE;

        if ShellExecuteExW(&mut info) == 0 {
            let err = GetLastError();
            // ERROR_CANCELLED 1223 = пользователь нажал «Нет» в UAC.
            let msg = if err == 1223 {
                "элевация отклонена пользователем (UAC: Нет)".to_owned()
            } else {
                format!("элевация не удалась (GetLastError={err})")
            };
            logging::error("elevate", &msg);
            return Err(msg);
        }
        if info.h_process.is_null() {
            logging::warn("elevate", "нет hProcess для ожидания результата");
            return Ok(0);
        }
        WaitForSingleObject(info.h_process, INFINITE);
        let mut code: u32 = 0;
        GetExitCodeProcess(info.h_process, &mut code);
        CloseHandle(info.h_process);
        logging::info("elevate", format!("реинвок завершён, код={code}"));
        Ok(code as i32)
    }
}
