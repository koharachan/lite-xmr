use std::process;

pub fn daemonize() {
    #[cfg(target_family = "unix")]
    {
        if unsafe { libc::fork() } != 0 {
            process::exit(0);
        }
        unsafe {
            libc::setsid();
        }
        if unsafe { libc::fork() } != 0 {
            process::exit(0);
        }
        unsafe {
            libc::chdir(b"/\0".as_ptr() as *const _);
        }
        let devnull = unsafe { libc::open(b"/dev/null\0".as_ptr() as *const _, libc::O_RDWR) };
        if devnull >= 0 {
            unsafe {
                libc::dup2(devnull, libc::STDIN_FILENO);
                libc::dup2(devnull, libc::STDOUT_FILENO);
                libc::dup2(devnull, libc::STDERR_FILENO);
                libc::close(devnull);
            }
        }
    }
}
