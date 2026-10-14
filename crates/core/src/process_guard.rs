//! 子进程防孤儿：应用退出（含被强杀）时连带结束它拉起的整棵进程树。
//!
//! Windows：把**当前进程**放进一个设了 `KILL_ON_JOB_CLOSE` 的 job object。
//! 之后拉起的子进程（以及 `cmd /c npx …` → node 这类孙进程）默认继承 job；
//! 本进程一旦结束，系统关闭最后一个 job 句柄，job 内所有进程随之被终止。
//! 这样不需要拿到协议库内部的 `Child` 句柄。
//!
//! 其他平台目前为空操作（子进程靠 stdin 关闭自行退出）。

/// 启用防孤儿保护。应在拉起任何子进程之前调用一次；重复调用无害。
pub fn kill_children_on_exit() -> std::io::Result<()> {
    #[cfg(windows)]
    {
        windows_impl::install()
    }
    #[cfg(not(windows))]
    {
        Ok(())
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::io;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    /// 安装结果只算一次；job 句柄故意不关闭——它要活到进程结束。
    static INSTALLED: OnceLock<Result<(), i32>> = OnceLock::new();

    pub fn install() -> io::Result<()> {
        INSTALLED
            .get_or_init(|| unsafe { create_and_assign() }.map_err(|e| e.raw_os_error().unwrap_or(-1)))
            .map_err(io::Error::from_raw_os_error)
    }

    unsafe fn create_and_assign() -> io::Result<()> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            // Windows 8+ 支持嵌套 job：即便本进程已在别的 job（如某些终端 / IDE）里也能成功
            if AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }
}
