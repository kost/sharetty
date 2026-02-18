use anyhow::Result;
use tokio::io::{AsyncRead, AsyncWrite};


#[cfg(unix)]
use pty_process as pty_impl;

#[cfg(windows)]
use winptyrs as pty_impl;

#[cfg(windows)]
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use tokio::sync::mpsc;

pub struct Pty {
    #[cfg(unix)]
    inner: pty_impl::Pty,
    #[cfg(windows)]
    inner: Arc<Mutex<pty_impl::PTY>>,
}

pub struct Child {
    #[cfg(unix)]
    inner: tokio::process::Child,
    #[cfg(windows)]
    inner: (),
}

impl Child {
    pub async fn kill(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            self.inner.kill().await
        }
        #[cfg(windows)]
        {
            // Windows PTY process lifetime is tied to PTY object generally,
            // or we need to access PTY to kill. 
            // Since we detached PTY into Arc, we might need a way to signal.
            // For now, no-op or we rely on session drop.
            Ok(())
        }
    }
}

pub struct PtyReader {
    #[cfg(unix)]
    inner: pty_impl::OwnedReadPty,
    #[cfg(windows)]
    inner: mpsc::Receiver<Vec<u8>>,
    #[cfg(windows)]
    buffer: Vec<u8>,
}

pub struct PtyWriter {
    #[cfg(unix)]
    inner: pty_impl::OwnedWritePty,
    #[cfg(windows)]
    pty: Arc<Mutex<pty_impl::PTY>>,
}

impl PtyWriter {
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
         #[cfg(unix)]
         {
             use pty_impl::Size;
             self.inner.resize(Size::new(rows, cols))?;
         }
         
         #[cfg(windows)]
         {
             let pty = self.pty.lock().unwrap();
             pty.set_size(cols as i32, rows as i32)
                .map_err(|e| anyhow::anyhow!("PTY resize error: {:?}", e))?;
         }
         Ok(())
    }
}

impl Pty {
    #[cfg(unix)]
    pub fn new(cmd: &str, args: &[String]) -> Result<(Self, Child)> {
        use pty_impl::Command;
        
        let mut command = Command::new(cmd);
        command.args(args);
        
        let pty = pty_impl::Pty::new()?;
        let pts = pty.pts()?;
        let child = command.spawn(&pts)?;
        
        Ok((Self { inner: pty }, Child { inner: child }))
    }

    #[cfg(windows)]
    pub fn new(cmd: &str, args: &[String]) -> Result<(Self, Child)> {
        use pty_impl::{PTYArgs, PTY, MouseMode, AgentConfig};
        use std::ffi::OsString;
        
        let pty_args = PTYArgs {
            cols: 80,
            rows: 24,
            mouse_mode: MouseMode::WINPTY_MOUSE_MODE_NONE,
            timeout: 10000,
            agent_config: AgentConfig::WINPTY_FLAG_COLOR_ESCAPES,
        };
        
        let mut pty = PTY::new(&pty_args)
            .map_err(|e| anyhow::anyhow!("PTY init error: {:?}", e))?;
        
        let mut cmd_line = OsString::from(cmd);
        for arg in args {
            cmd_line.push(" ");
            cmd_line.push(arg);
        }
        
        let appname = OsString::from(cmd);

        // WinPTY spawn signature: spawn(appname, cmdline, cwd, env)
        pty.spawn(appname, Some(cmd_line), None, None)
             .map_err(|e| anyhow::anyhow!("PTY spawn error: {:?}", e))?;
        
        Ok((Self { inner: Arc::new(Mutex::new(pty)) }, Child { inner: () }))
    }

    pub fn into_split(self) -> (PtyReader, PtyWriter) {
        #[cfg(unix)]
        {
            let (read, write) = self.inner.into_split();
            (PtyReader { inner: read }, PtyWriter { inner: write })
        }
        
        #[cfg(windows)]
        {
            let pty_reader = self.inner.clone();
            let pty_writer = self.inner.clone();
            
            // Create channel
            let (tx, rx) = mpsc::channel(100);
            
            // Spawn reader thread
            std::thread::spawn(move || {
                loop {
                    let mut data = None;
                    {
                        if let Ok(pty) = pty_reader.lock() {
                            let pty: &pty_impl::PTY = &*pty;
                            let is_alive: bool = pty.is_alive().unwrap_or(false);
                            if !is_alive {
                                break;
                            }
                            // Non-blocking read
                            match pty.read(4096, false) {
                                Ok(s) => {
                                    let s: std::ffi::OsString = s;
                                    let s_cow: std::borrow::Cow<str> = s.to_string_lossy();
                                    if !s_cow.is_empty() {
                                        data = Some(s_cow.into_owned().into_bytes());
                                    }
                                }
                                Err(_) => {} // Ignore errors or break?
                            }
                        }
                    } // Unlock
                    
                    if let Some(d) = data {
                        if tx.blocking_send(d).is_err() {
                            break; // Receiver dropped
                        }
                    } else {
                        // Sleep to avoid busy loop
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            });
            
            (
                PtyReader { inner: rx, buffer: Vec::new() }, 
                PtyWriter { pty: pty_writer }
            )
        }
    }
}

#[cfg(unix)]
impl AsyncRead for PtyReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

#[cfg(windows)]
impl AsyncRead for PtyReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        // If we have buffered data, use it
        if !self.buffer.is_empty() {
            let len = std::cmp::min(self.buffer.len(), buf.remaining());
            buf.put_slice(&self.buffer[..len]);
            self.buffer.drain(..len);
            return std::task::Poll::Ready(Ok(()));
        }

        // Poll channel
        match self.inner.poll_recv(cx) {
            std::task::Poll::Ready(Some(data)) => {
                let len = std::cmp::min(data.len(), buf.remaining());
                buf.put_slice(&data[..len]);
                if len < data.len() {
                    self.buffer.extend_from_slice(&data[len..]);
                }
                std::task::Poll::Ready(Ok(()))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(Ok(())), // EOF
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

#[cfg(unix)]
impl AsyncWrite for PtyWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(windows)]
impl AsyncWrite for PtyWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let pty = self.pty.lock().unwrap();
        // Convert buf to OsString (best effort)
        let s = String::from_utf8_lossy(buf);
        let os_s = OsString::from(s.into_owned());
        
        match pty.write(os_s) {
            Ok(_) => std::task::Poll::Ready(Ok(buf.len())),
            Err(e) => std::task::Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, format!("{:?}", e)))),
        }
    }
    
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}
