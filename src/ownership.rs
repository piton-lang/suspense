//! Who owns what Suspense creates: the user running it, as though they had
//! made it themselves, as the ApplicationScope's file ownership says.
//!
//! On Linux and macOS a process creates what it creates as its own user, and
//! on Linux containers run with `--userns=keep-id` (see [`crate::container`]),
//! so nothing is needed there. Windows' build declares it runs as invoker,
//! never asking for elevation (gpui's embedded manifest does so). Should it be
//! started elevated all the same, as from an administrator's terminal, what an
//! elevated process creates is owned by the Administrators group: the token's
//! default owner is. So the process's token is made to give the signed-in
//! user as the owner instead, before anything is created. Every program it
//! starts, git and the harness among them, is given a copy of that token, so
//! what they create is the user's too, and each file still takes the
//! permissions it inherits from its folder.

/// Makes the user the owner of everything this process, and every program it
/// starts, creates from now on, where it was started elevated on Windows;
/// does nothing elsewhere.
pub fn own_what_is_created() {
    #[cfg(windows)]
    if let Err(err) = windows::own_what_is_created() {
        eprintln!("couldn't make the user the owner of what Suspense creates: {err}");
    }
}

#[cfg(windows)]
mod windows {
    use std::mem::size_of;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ELEVATION,
        TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenOwner, TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// The process's token, closed when dropped.
    struct Token(HANDLE);

    impl Drop for Token {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    pub fn own_what_is_created() -> Result<(), String> {
        let failed = |what: &str| format!("{what} failed ({})", unsafe { GetLastError() });
        let mut handle: HANDLE = null_mut();
        if unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_ADJUST_DEFAULT,
                &mut handle,
            )
        } == 0
        {
            return Err(failed("OpenProcessToken"));
        }
        let token = Token(handle);

        // Not elevated, what it creates is already the user's.
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = 0u32;
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast(),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            )
        } == 0
        {
            return Err(failed("GetTokenInformation(TokenElevation)"));
        }
        if elevation.TokenIsElevated == 0 {
            return Ok(());
        }

        // The signed-in user, as the token names them.
        unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut size) };
        if size == 0 {
            return Err(failed("GetTokenInformation(TokenUser) size"));
        }
        // Aligned for the TOKEN_USER at its start.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        } == 0
        {
            return Err(failed("GetTokenInformation(TokenUser)"));
        }
        let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };

        // Everything created from now on is owned by them.
        let owner = TOKEN_OWNER {
            Owner: user.User.Sid,
        };
        if unsafe {
            SetTokenInformation(
                token.0,
                TokenOwner,
                (&owner as *const TOKEN_OWNER).cast(),
                size_of::<TOKEN_OWNER>() as u32,
            )
        } == 0
        {
            return Err(failed("SetTokenInformation(TokenOwner)"));
        }
        Ok(())
    }
}
