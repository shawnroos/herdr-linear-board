//! The two platform side effects Linear mode performs outside the daemon:
//! opening an issue URL and copying a worktree path. Injected on the driver
//! (like `EditorLauncher`) so tests stub them.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use base64::prelude::{Engine as _, BASE64_STANDARD as STANDARD};

pub trait PlatformActions {
    fn open_url(&mut self, url: &str) -> Result<()>;
    fn copy_text(&mut self, text: &str) -> Result<()>;
}

pub struct RealPlatform;

impl PlatformActions for RealPlatform {
    fn open_url(&mut self, url: &str) -> Result<()> {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let mut child = Command::new(opener)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("running {opener}"))?;
        // Reap off the TUI thread: the opener may outlive the key press.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    fn copy_text(&mut self, text: &str) -> Result<()> {
        // OSC 52 reaches the terminal the board runs in, including a remote
        // one; the clipboard tools below only see the local machine.
        let mut stdout = std::io::stdout();
        write!(stdout, "\x1b]52;c;{}\x07", STANDARD.encode(text.as_bytes()))?;
        stdout.flush()?;
        for argv in [
            &["pbcopy"][..],
            &["xclip", "-selection", "clipboard"][..],
            &["wl-copy"][..],
        ] {
            let Ok(mut child) = Command::new(argv[0])
                .args(&argv[1..])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                continue;
            };
            // Fed and reaped off the TUI thread: a clipboard helper wedged on
            // a broken X or Wayland session would otherwise freeze the board.
            // OSC 52 above has already copied for any terminal that honours it.
            let stdin = child.stdin.take();
            let owned = text.to_string();
            std::thread::spawn(move || {
                if let Some(mut stdin) = stdin {
                    let _ = stdin.write_all(owned.as_bytes());
                }
                let _ = child.wait();
            });
            break;
        }
        Ok(())
    }
}
