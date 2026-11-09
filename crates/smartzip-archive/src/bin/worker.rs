fn main() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
        if libc::getppid() == 1 {
            std::process::exit(1);
        }
    }
    if let Err(error) = smartzip_archive::worker::run() {
        eprintln!("archive worker: {error}");
        std::process::exit(1);
    }
}
